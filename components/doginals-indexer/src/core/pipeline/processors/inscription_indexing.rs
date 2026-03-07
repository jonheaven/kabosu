use std::{
    collections::{BTreeMap, HashMap},
    hash::BuildHasherDefault,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use dogecoin::{
    try_info, try_warn,
    types::{DogecoinBlockData, TransactionBytesCursor, TransactionIdentifier},
    utils::Context,
};
use config::Config;
use dashmap::DashMap;
use fxhash::FxHasher;
use postgres::{pg_begin, pg_pool_client};

use crate::{
    core::{
        meta_protocols::drc20::{
            drc20_pg, cache::Brc20MemoryCache, index::index_block_and_insert_drc20_operations,
        },
        protocol::{
            inscription_parsing::{
                parse_inscriptions_in_standardized_block, ParsedLottoDeploy, ParsedLottoMint,
            },
            inscription_sequencing::{
                get_dogecoin_network, get_jubilee_block_height,
                parallelize_inscription_data_computations,
                update_block_inscriptions_with_consensus_sequence_data,
            },
            koinu_numbering::TraversalResult,
            koinu_tracking::augment_block_with_transfers,
            sequence_cursor::SequenceCursor,
        },
    },
    db::doginals_pg::{self, get_chain_tip_block_height},
    utils::{monitoring::PrometheusMonitoring, webhooks},
    PgConnectionPools,
};

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub async fn process_blocks(
    next_blocks: &mut Vec<DogecoinBlockData>,
    sequence_cursor: &mut SequenceCursor,
    cache_l2: &Arc<DashMap<(u32, [u8; 8]), TransactionBytesCursor, BuildHasherDefault<FxHasher>>>,
    drc20_cache: &mut Option<Brc20MemoryCache>,
    prometheus: &PrometheusMonitoring,
    config: &Config,
    pg_pools: &PgConnectionPools,
    ctx: &Context,
    abort_signal: &Arc<AtomicBool>,
) -> Result<Vec<DogecoinBlockData>, String> {
    let mut cache_l1 = BTreeMap::new();
    let mut updated_blocks = vec![];

    for _cursor in 0..next_blocks.len() {
        if abort_signal.load(Ordering::SeqCst) {
            break;
        }
        let mut block = next_blocks.remove(0);

        index_block(
            &mut block,
            next_blocks,
            sequence_cursor,
            &mut cache_l1,
            cache_l2,
            drc20_cache.as_mut(),
            prometheus,
            config,
            pg_pools,
            ctx,
        )
        .await?;

        updated_blocks.push(block);
    }

    Ok(updated_blocks)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub async fn index_block(
    block: &mut DogecoinBlockData,
    next_blocks: &[DogecoinBlockData],
    sequence_cursor: &mut SequenceCursor,
    cache_l1: &mut BTreeMap<(TransactionIdentifier, usize, u64), TraversalResult>,
    cache_l2: &Arc<DashMap<(u32, [u8; 8]), TransactionBytesCursor, BuildHasherDefault<FxHasher>>>,
    drc20_cache: Option<&mut Brc20MemoryCache>,
    prometheus: &PrometheusMonitoring,
    config: &Config,
    pg_pools: &PgConnectionPools,
    ctx: &Context,
) -> Result<(), String> {
    let stopwatch = std::time::Instant::now();
    let block_height = block.block_identifier.index;
    try_info!(
        ctx,
        "Starting inscription indexing for block #{block_height}..."
    );
    // Count total reveals and transfers in the block
    let mut reveals_count = 0;
    let mut transfers_count = 0;

    // Invalidate and recompute cursor when crossing the jubilee height
    if block.block_identifier.index
        == get_jubilee_block_height(&get_dogecoin_network(&block.metadata.network))
    {
        sequence_cursor.reset();
    }

    {
        let mut ord_client = pg_pool_client(&pg_pools.ordinals).await?;
        let ord_tx = pg_begin(&mut ord_client).await?;

        if let Some(chain_tip) = get_chain_tip_block_height(&ord_tx).await? {
            if block_height <= chain_tip {
                try_warn!(ctx, "Block #{block_height} was already indexed, skipping");
                return Ok(());
            }
        }

        // Parsed DRC20 ops will be deposited here for this block.
        let mut drc20_operation_map = HashMap::new();
        // DNS name → inscription_id (first wins within block)
        let mut dns_map: HashMap<String, String> = HashMap::new();
        // Dogemap block_number → inscription_id (first wins within block)
        let mut dogemap_map: HashMap<u32, String> = HashMap::new();
        let mut lotto_deploy_map: HashMap<String, ParsedLottoDeploy> = HashMap::new();
        let mut lotto_mints: Vec<ParsedLottoMint> = Vec::new();

        // Measure inscription parsing time
        let parsing_start = std::time::Instant::now();
        parse_inscriptions_in_standardized_block(
            block,
            &mut drc20_operation_map,
            &mut dns_map,
            &mut dogemap_map,
            &mut lotto_deploy_map,
            &mut lotto_mints,
            config,
            ctx,
        );
        prometheus
            .metrics_record_inscription_parsing_time(parsing_start.elapsed().as_millis() as f64);

        // Measure ordinal computation time
        let computation_start = std::time::Instant::now();
        let has_inscription_reveals = match parallelize_inscription_data_computations(
            block,
            next_blocks,
            cache_l1,
            cache_l2,
            config,
            ctx,
        ) {
            Ok(result) => result,
            Err(e) => {
                return Err(format!("Failed to compute inscription data: {}", e));
            }
        };
        if has_inscription_reveals {
            if let Err(e) = update_block_inscriptions_with_consensus_sequence_data(
                block,
                sequence_cursor,
                cache_l1,
                &ord_tx,
                ctx,
            )
            .await
            {
                return Err(format!("Failed to update block inscriptions: {}", e));
            }
        }
        prometheus.metrics_record_inscription_computation_time(
            computation_start.elapsed().as_millis() as f64,
        );

        if let Err(e) = augment_block_with_transfers(
            block,
            &ord_tx,
            ctx,
            &mut reveals_count,
            &mut transfers_count,
        )
        .await
        {
            return Err(format!("Failed to augment block with transfers: {}", e));
        }

        // Count inscriptions revealed in this block
        prometheus.metrics_record_inscriptions_per_block(reveals_count as u64);

        // Measure database write time
        let inscription_db_write_start = std::time::Instant::now();
        // Write data
        if let Err(e) = doginals_pg::insert_block(block, &ord_tx).await {
            return Err(format!("Failed to insert block: {}", e));
        }

        // DNS — write name registrations detected in this block
        if !dns_map.is_empty() {
            if let Err(e) = doginals_pg::insert_dns_names(&dns_map, block_height, block.timestamp, &ord_tx).await {
                return Err(format!("Failed to insert DNS names: {}", e));
            }
            try_info!(ctx, "Indexed {} DNS name(s) at block #{block_height}", dns_map.len());
            let webhook_urls = config.webhook_urls().to_vec();
            if !webhook_urls.is_empty() {
                for (name, inscription_id) in &dns_map {
                    let payload = webhooks::dns_event(name, inscription_id, block_height, block.timestamp);
                    webhooks::fire_webhooks(&webhook_urls, payload, ctx).await;
                }
            }
        }

        // Dogemap — write block claims detected in this block
        if !dogemap_map.is_empty() {
            if let Err(e) = doginals_pg::insert_dogemap_claims(&dogemap_map, block_height, block.timestamp, &ord_tx).await {
                return Err(format!("Failed to insert Dogemap claims: {}", e));
            }
            try_info!(ctx, "Indexed {} Dogemap claim(s) at block #{block_height}", dogemap_map.len());
            let webhook_urls = config.webhook_urls().to_vec();
            if !webhook_urls.is_empty() {
                for (block_number, inscription_id) in &dogemap_map {
                    let payload = webhooks::dogemap_event(*block_number, inscription_id, block_height, block.timestamp);
                    webhooks::fire_webhooks(&webhook_urls, payload, ctx).await;
                }
            }
        }

        if !lotto_deploy_map.is_empty() {
            if let Err(e) = doginals_pg::insert_lotto_lotteries(
                &lotto_deploy_map,
                block_height,
                block.timestamp,
                &ord_tx,
            )
            .await
            {
                return Err(format!("Failed to insert doge-lotto deploys: {}", e));
            }
        }

        let inserted_lotto_tickets = if !lotto_mints.is_empty() {
            // Ticket insertion re-validates the mint against the stored deploy and
            // checks the prize-pool payment on this same transaction's outputs.
            match doginals_pg::insert_lotto_tickets(
                &lotto_mints,
                block_height,
                block.timestamp,
                &ord_tx,
            )
            .await
            {
                Ok(rows) => rows,
                Err(e) => return Err(format!("Failed to insert doge-lotto tickets: {}", e)),
            }
        } else {
            Vec::new()
        };

        let resolved_lotto_winners = doginals_pg::resolve_lotto(
            block_height,
            &block.block_identifier.hash,
            block.timestamp,
            &ord_tx,
        )
        .await
        .map_err(|e| format!("Failed to resolve doge-lotto draws: {}", e))?;

        let webhook_urls = config.webhook_urls().to_vec();
        if !webhook_urls.is_empty() {
            for ticket in &inserted_lotto_tickets {
                let payload = webhooks::lotto_ticket_event(
                    &ticket.lotto_id,
                    &ticket.ticket_id,
                    &ticket.inscription_id,
                    &ticket.tx_id,
                    ticket.minted_height,
                    ticket.minted_timestamp,
                    &ticket.seed_numbers,
                );
                webhooks::fire_webhooks(&webhook_urls, payload, ctx).await;
            }
            for winner in &resolved_lotto_winners {
                let payload = webhooks::lotto_winner_event(
                    &winner.lotto_id,
                    &winner.ticket_id,
                    &winner.inscription_id,
                    winner.resolved_height,
                    winner.rank,
                    winner.score,
                    winner.payout_bps,
                    winner.payout_koinu,
                    &winner.seed_numbers,
                    &winner.drawn_numbers,
                );
                webhooks::fire_webhooks(&webhook_urls, payload, ctx).await;
            }
        }

        if !lotto_deploy_map.is_empty() || !inserted_lotto_tickets.is_empty() || !resolved_lotto_winners.is_empty() {
            try_info!(
                ctx,
                "doge-lotto at block #{block_height}: {} deploy(s), {} ticket mint(s), {} winner record(s)",
                lotto_deploy_map.len(),
                inserted_lotto_tickets.len(),
                resolved_lotto_winners.len(),
            );
        }

        prometheus.metrics_record_inscription_db_write_time(
            inscription_db_write_start.elapsed().as_millis() as f64,
        );

        // BRC-20
        if let (Some(drc20_cache), Some(drc20_pool)) = (drc20_cache, &pg_pools.drc20) {
            let mut drc20_client = pg_pool_client(drc20_pool).await?;
            let drc20_tx = pg_begin(&mut drc20_client).await?;

            // Count BRC-20 operations before processing
            let drc20_ops_count = drc20_operation_map.len() as u64;
            prometheus.metrics_record_drc20_operations_per_block(drc20_ops_count);

            if let Err(e) = index_block_and_insert_drc20_operations(
                block,
                &mut drc20_operation_map,
                drc20_cache,
                &drc20_tx,
                ctx,
                prometheus,
            )
            .await
            {
                return Err(format!("Failed to process BRC-20 operations: {}", e));
            }

            if let Err(e) = drc20_tx.commit().await {
                return Err(format!("unable to commit drc20 pg transaction: {}", e));
            }
        }

        prometheus.metrics_block_indexed(block_height);
        prometheus.metrics_inscription_indexed(
            doginals_pg::get_highest_inscription_number(&ord_tx)
                .await?
                .unwrap_or(0) as u64,
        );
        prometheus.metrics_classic_blessed_inscription_indexed(
            doginals_pg::get_blessed_count_from_counts_by_type(&ord_tx)
                .await?
                .unwrap_or(0) as u64,
        );
        prometheus.metrics_classic_cursed_inscription_indexed(
            doginals_pg::get_cursed_count_from_counts_by_type(&ord_tx)
                .await?
                .unwrap_or(0) as u64,
        );

        if let Err(e) = ord_tx.commit().await {
            return Err(format!("unable to commit ordinals pg transaction: {}", e));
        }
    }

    // Record overall processing time
    let elapsed = stopwatch.elapsed();
    prometheus.metrics_record_block_processing_time(elapsed.as_millis() as f64);
    try_info!(
        ctx,
        "Completed inscription indexing for block #{block_height}: found {reveals_count} inscription reveals and {transfers_count} inscription transfers in {elapsed:.0}s",
        elapsed = elapsed.as_secs_f32(),
    );

    Ok(())
}

pub async fn rollback_block(
    block_height: u64,
    _config: &Config,
    pg_pools: &PgConnectionPools,
    ctx: &Context,
) -> Result<(), String> {
    try_info!(ctx, "Rolling back block #{block_height}");
    {
        let mut ord_client = pg_pool_client(&pg_pools.ordinals).await?;
        let ord_tx = pg_begin(&mut ord_client).await?;

        doginals_pg::rollback_block(block_height, &ord_tx).await?;
        doginals_pg::rollback_dns_names(block_height, &ord_tx).await?;
        doginals_pg::rollback_dogemap_claims(block_height, &ord_tx).await?;
        doginals_pg::rollback_lotto_resolutions(block_height, &ord_tx).await?;
        doginals_pg::rollback_lotto_tickets(block_height, &ord_tx).await?;
        doginals_pg::rollback_lotto_lotteries(block_height, &ord_tx).await?;

        // BRC-20
        if let Some(drc20_pool) = &pg_pools.drc20 {
            let mut drc20_client = pg_pool_client(drc20_pool).await?;
            let drc20_tx = pg_begin(&mut drc20_client).await?;

            drc20_pg::rollback_block_operations(block_height, &drc20_tx).await?;

            drc20_tx
                .commit()
                .await
                .map_err(|e| format!("unable to commit drc20 pg transaction: {e}"))?;
            try_info!(
                ctx,
                "Rolled back BRC-20 operations at block #{block_height}"
            );
        }

        ord_tx
            .commit()
            .await
            .map_err(|e| format!("unable to commit ordinals pg transaction: {e}"))?;
        try_info!(
            ctx,
            "Rolled back inscription activity at block #{block_height}"
        );
    }
    Ok(())
}
