use std::{
    collections::{BTreeMap, HashMap},
    hash::BuildHasherDefault,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use config::Config;
use dashmap::DashMap;
use dogecoin::{
    try_info, try_warn,
    types::{DogecoinBlockData, OperationType, TransactionBytesCursor, TransactionIdentifier},
    utils::Context,
};
use fxhash::FxHasher;
use postgres::{pg_begin, pg_pool_client};

use crate::{
    core::{
        meta_protocols::{
            dogespells::{identity_hex, try_parse_dogespells_spell, IndexedDogeSpellsSpell},
            dogetag::try_parse_dogetag,
            drc20::{
                cache::Brc20MemoryCache, drc20_pg, index::index_block_and_insert_drc20_operations,
            },
        },
        protocol::{
            inscription_parsing::{
                parse_inscriptions_in_standardized_block, ParsedDmpOp, ParsedLottoDeploy,
                ParsedLottoMint,
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
    db::{
        checkpoint,
        doginals_pg::{self, get_chain_tip_block_height},
    },
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
        let mut dmp_ops: Vec<ParsedDmpOp> = Vec::new();
        // Dogetags: (txid, sender_address, message, raw_script)
        let mut dogetag_list: Vec<(String, Option<String>, String, String)> = Vec::new();
        let mut dogespells_list: Vec<IndexedDogeSpellsSpell> = Vec::new();

        // Measure inscription parsing time
        let parsing_start = std::time::Instant::now();
        parse_inscriptions_in_standardized_block(
            block,
            &mut drc20_operation_map,
            &mut dns_map,
            &mut dogemap_map,
            &mut lotto_deploy_map,
            &mut lotto_mints,
            &mut dmp_ops,
            config,
            ctx,
        );
        prometheus
            .metrics_record_inscription_parsing_time(parsing_start.elapsed().as_millis() as f64);

        // Dogetag scan — check every tx output for OP_RETURN graffiti.
        if config.dogetag_enabled() {
            for tx in &block.transactions {
                let txid = tx.transaction_identifier.get_hash_bytes_str().to_string();
                // Attempt to extract sender address from the first Debit operation (spender).
                let sender_address: Option<String> = tx
                    .operations
                    .iter()
                    .find(|op| op.type_ == OperationType::Debit)
                    .map(|op| op.account.address.clone());

                for output in &tx.metadata.outputs {
                    if let Some(message) = try_parse_dogetag(&output.script_pubkey) {
                        dogetag_list.push((
                            txid.clone(),
                            sender_address.clone(),
                            message,
                            output.script_pubkey.clone(),
                        ));
                        break; // one tag per transaction
                    }
                }
            }
        }

        // DogeSpells scan - OP_RETURN payloads with the DogeSpells magic prefix followed by CBOR.
        // Invalid or malformed payloads are ignored silently, matching Dogetag's behavior.
        if config.dogespells_enabled() {
            for tx in &block.transactions {
                let txid = tx.transaction_identifier.get_hash_bytes_str().to_string();

                for (vout, output) in tx.metadata.outputs.iter().enumerate() {
                    let Some(indexed) = try_parse_dogespells_spell(&output.script_pubkey) else {
                        continue;
                    };

                    let spell_txid = indexed.spell.txid.trim_start_matches("0x");
                    if !spell_txid.eq_ignore_ascii_case(&txid)
                        || indexed.spell.vout != vout as u32
                        || indexed.spell.block_height != block_height
                        || indexed.spell.block_timestamp != block.timestamp
                    {
                        continue;
                    }

                    dogespells_list.push(indexed);
                }
            }
        }

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

        // Emit explicit logs for every inscription reveal indexed in this block.
        if reveals_count > 0 {
            let reveal_rows = ord_tx
                .query(
                    "SELECT inscription_id, tx_id, number, content_type, COALESCE(address, '')
                     FROM inscriptions
                     WHERE block_height::bigint = $1
                     ORDER BY number ASC",
                    &[&(block_height as i64)],
                )
                .await
                .map_err(|e| format!("Failed to query inscription reveals for logging: {}", e))?;

            for row in reveal_rows {
                let inscription_id: String = row.get(0);
                let tx_id: String = row.get(1);
                let number: i64 = row.get(2);
                let content_type: String = row.get(3);
                let address: String = row.get(4);
                let owner = if address.is_empty() {
                    "unknown"
                } else {
                    address.as_str()
                };
                try_info!(
                    ctx,
                    "Inscription reveal: #{} id={} tx={} owner={} content_type={}",
                    number,
                    inscription_id,
                    tx_id,
                    owner,
                    content_type
                );
            }
        }

        // Emit explicit logs for every inscription transfer indexed in this block.
        if transfers_count > 0 {
            let transfer_rows = ord_tx
                .query(
                    "SELECT inscription_id, number, tx_index, block_transfer_index
                     FROM inscription_transfers
                     WHERE block_height::bigint = $1
                     ORDER BY block_transfer_index ASC",
                    &[&(block_height as i64)],
                )
                .await
                .map_err(|e| format!("Failed to query inscription transfers for logging: {}", e))?;

            for row in transfer_rows {
                let inscription_id: String = row.get(0);
                let number: i64 = row.get(1);
                let tx_index: i64 = row.get(2);
                let block_transfer_index: i32 = row.get(3);
                try_info!(
                    ctx,
                    "Inscription transfer: #{} id={} tx_index={} transfer_index={}",
                    number,
                    inscription_id,
                    tx_index,
                    block_transfer_index
                );
            }
        }

        // DNS — write name registrations detected in this block
        if !dns_map.is_empty() {
            if let Err(e) =
                doginals_pg::insert_dns_names(&dns_map, block_height, block.timestamp, &ord_tx)
                    .await
            {
                return Err(format!("Failed to insert DNS names: {}", e));
            }
            try_info!(
                ctx,
                "Indexed {} DNS name(s) at block #{block_height}",
                dns_map.len()
            );
            for (name, inscription_id) in dns_map.iter().take(5) {
                try_info!(ctx, "DNS claim: {} <- {}", name, inscription_id);
            }
            let webhook_urls = config.webhook_urls().to_vec();
            if !webhook_urls.is_empty() {
                for (name, inscription_id) in &dns_map {
                    let payload =
                        webhooks::dns_event(name, inscription_id, block_height, block.timestamp);
                    webhooks::fire_webhooks(
                        webhook_urls.clone(),
                        config.webhooks.hmac_secret.clone(),
                        payload,
                    );
                }
            }
        }

        // Dogemap — write block claims detected in this block
        if !dogemap_map.is_empty() {
            if let Err(e) = doginals_pg::insert_dogemap_claims(
                &dogemap_map,
                block_height,
                block.timestamp,
                &ord_tx,
            )
            .await
            {
                return Err(format!("Failed to insert Dogemap claims: {}", e));
            }
            try_info!(
                ctx,
                "Indexed {} Dogemap claim(s) at block #{block_height}",
                dogemap_map.len()
            );
            for (block_number, inscription_id) in dogemap_map.iter().take(5) {
                try_info!(
                    ctx,
                    "Dogemap claim: block {} <- {}",
                    block_number,
                    inscription_id
                );
            }
            let webhook_urls = config.webhook_urls().to_vec();
            if !webhook_urls.is_empty() {
                for (block_number, inscription_id) in &dogemap_map {
                    let payload = webhooks::dogemap_event(
                        *block_number,
                        inscription_id,
                        block_height,
                        block.timestamp,
                    );
                    webhooks::fire_webhooks(
                        webhook_urls.clone(),
                        config.webhooks.hmac_secret.clone(),
                        payload,
                    );
                }
            }
        }

        // Dogetags — write all tags found in this block
        if !dogetag_list.is_empty() {
            if let Err(e) =
                doginals_pg::insert_dogetags(&dogetag_list, block_height, block.timestamp, &ord_tx)
                    .await
            {
                return Err(format!("Failed to insert dogetags: {}", e));
            }
            try_info!(
                ctx,
                "Indexed {} dogetag(s) at block #{block_height}",
                dogetag_list.len()
            );
            for (txid, sender, message, _) in dogetag_list.iter().take(5) {
                let short_txid = if txid.len() > 16 {
                    &txid[..16]
                } else {
                    txid.as_str()
                };
                let message_preview: String = message.chars().take(80).collect();
                try_info!(
                    ctx,
                    "Dogetag: tx={} sender={} message=\"{}\"",
                    short_txid,
                    sender.as_deref().unwrap_or("unknown"),
                    message_preview
                );
            }
            let webhook_urls = config.webhook_urls().to_vec();
            if !webhook_urls.is_empty() {
                for (txid, sender, message, _) in &dogetag_list {
                    let payload = webhooks::dogetag_event(
                        txid,
                        sender.as_deref().unwrap_or(""),
                        message,
                        block_height,
                        block.timestamp,
                    );
                    webhooks::fire_webhooks(
                        webhook_urls.clone(),
                        config.webhooks.hmac_secret.clone(),
                        payload,
                    );
                }
            }
        }

        if !dogespells_list.is_empty() {
            if let Err(e) = doginals_pg::insert_dogespells(&dogespells_list, &ord_tx).await {
                return Err(format!("Failed to insert dogespells spells: {}", e));
            }
            try_info!(
                ctx,
                "Indexed {} DogeSpells spell(s) at block #{block_height}",
                dogespells_list.len()
            );
            for indexed in dogespells_list.iter().take(5) {
                let spell = &indexed.spell;
                let short_txid = if spell.txid.len() > 16 {
                    &spell.txid[..16]
                } else {
                    spell.txid.as_str()
                };
                try_info!(
                    ctx,
                    "DogeSpells spell: op={} tag={} identity={} ticker={} tx={}#{}",
                    spell.op,
                    spell.tag,
                    identity_hex(&spell.id),
                    spell.ticker.as_deref().unwrap_or("-"),
                    short_txid,
                    spell.vout,
                );
            }

            let webhook_urls = config.webhook_urls().to_vec();
            if !webhook_urls.is_empty() {
                for indexed in &dogespells_list {
                    let spell = &indexed.spell;
                    let payload = match spell.op.as_str() {
                        "mint" => Some(webhooks::dogespells_mint_event(spell)),
                        "transfer" => Some(webhooks::dogespells_transfer_event(spell)),
                        "burn" => Some(webhooks::dogespells_burn_event(spell)),
                        "beam_out" => Some(webhooks::dogespells_beam_out_event(spell)),
                        "beam_in" => Some(webhooks::dogespells_beam_in_event(spell)),
                        _ => None,
                    };

                    if let Some(payload) = payload {
                        webhooks::fire_webhooks(
                            webhook_urls.clone(),
                            config.webhooks.hmac_secret.clone(),
                            payload,
                        );
                    }
                }
            }
        }

        // DMP — index all market operations detected in this block
        if !dmp_ops.is_empty() {
            if let Err(e) = doginals_pg::insert_dmp_ops(&dmp_ops, &ord_tx).await {
                return Err(format!("Failed to insert DMP operations: {}", e));
            }
            try_info!(
                ctx,
                "Indexed {} DMP operation(s) at block #{block_height}",
                dmp_ops.len()
            );
            let webhook_urls = config.webhook_urls().to_vec();
            if !webhook_urls.is_empty() {
                for parsed in &dmp_ops {
                    let payload = webhooks::dmp_event(parsed);
                    webhooks::fire_webhooks(
                        webhook_urls.clone(),
                        config.webhooks.hmac_secret.clone(),
                        payload,
                    );
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
                return Err(format!("Failed to insert DogeLotto deploys: {}", e));
            }

            for (lotto_id, deploy) in &lotto_deploy_map {
                try_info!(
                    ctx,
                    "DogeLotto deploy: id={} template={:?} draw_block={} cutoff_block={} ticket_price_koinu={}",
                    lotto_id,
                    deploy.deploy.template,
                    deploy.deploy.draw_block,
                    deploy.deploy.cutoff_block,
                    deploy.deploy.ticket_price_koinu,
                );
            }
        }

        // Validate each mint against deploy + same-tx payment before DB insertion.
        let mut verified_lotto_mints: Vec<ParsedLottoMint> = Vec::new();
        for parsed in lotto_mints.into_iter() {
            let deploy = if let Some(local) = lotto_deploy_map.get(&parsed.mint.lotto_id) {
                Some(local.deploy.clone())
            } else {
                match doginals_pg::get_lotto_deploy_by_id(&parsed.mint.lotto_id, &ord_tx).await {
                    Ok(value) => value,
                    Err(e) => {
                        try_warn!(
                            ctx,
                            "Rejected DogeLotto mint {} ({}): unable to load deploy: {}",
                            parsed.inscription_id,
                            parsed.mint.lotto_id,
                            e
                        );
                        continue;
                    }
                }
            };

            let Some(deploy) = deploy else {
                try_warn!(
                    ctx,
                    "Rejected DogeLotto mint {} ({}): deploy not found",
                    parsed.inscription_id,
                    parsed.mint.lotto_id
                );
                continue;
            };

            if block_height > deploy.cutoff_block {
                try_warn!(
                    ctx,
                    "Rejected DogeLotto mint {} ({}): mint height {} exceeds cutoff {}",
                    parsed.inscription_id,
                    parsed.mint.lotto_id,
                    block_height,
                    deploy.cutoff_block
                );
                continue;
            }

            if !crate::core::meta_protocols::lotto::validate_mint_against_deploy(
                &parsed.mint,
                &deploy,
            ) {
                try_warn!(
                    ctx,
                    "Rejected DogeLotto mint {} ({}): seed numbers invalid for deploy config",
                    parsed.inscription_id,
                    parsed.mint.lotto_id
                );
                continue;
            }

            let (payment_ok, reason) = doginals_pg::verify_lotto_payment(
                &parsed,
                &deploy,
                &config.protocols.lotto.protocol_dev_address,
            );
            if !payment_ok {
                try_warn!(
                    ctx,
                    "Rejected DogeLotto mint {} ({}): {}",
                    parsed.inscription_id,
                    parsed.mint.lotto_id,
                    reason
                );
                continue;
            }

            verified_lotto_mints.push(parsed);
        }

        let inserted_lotto_tickets = if !verified_lotto_mints.is_empty() {
            // Ticket insertion re-validates the mint against the stored deploy and
            // checks the prize-pool payment + immutable tip payment on this same tx,
            // and enforces persisted deploy cutoff_block.
            match doginals_pg::insert_lotto_tickets(
                &verified_lotto_mints,
                block_height,
                block.timestamp,
                &config.protocols.lotto.protocol_dev_address,
                &ord_tx,
            )
            .await
            {
                Ok(rows) => rows,
                Err(e) => return Err(format!("Failed to insert DogeLotto tickets: {}", e)),
            }
        } else {
            Vec::new()
        };

        for ticket in &inserted_lotto_tickets {
            try_info!(
                ctx,
                "DogeLotto ticket: lotto_id={} ticket_id={} inscription_id={} tip_percent={} minted_height={}",
                ticket.lotto_id,
                ticket.ticket_id,
                ticket.inscription_id,
                ticket.tip_percent,
                ticket.minted_height,
            );
        }

        let resolved_lotto_winners = doginals_pg::resolve_lotto(
            block_height,
            &block.block_identifier.hash,
            block.timestamp,
            &ord_tx,
        )
        .await
        .map_err(|e| format!("Failed to resolve DogeLotto draws: {}", e))?;

        for winner in &resolved_lotto_winners {
            try_info!(
                ctx,
                "DogeLotto winner: lotto_id={} ticket_id={} rank={} payout_koinu={} gross_koinu={} tip_deduction_koinu={}",
                winner.lotto_id,
                winner.ticket_id,
                winner.rank,
                winner.payout_koinu,
                winner.gross_payout_koinu,
                winner.tip_deduction_koinu,
            );
        }

        // Burners: Detect lotto ticket burns (transfers to burn address)
        let burn_events = detect_lotto_burns(
            block,
            &config.protocols.lotto.burn_address,
            block_height,
            block.timestamp,
            &ord_tx,
            ctx,
        )
        .await?;

        for (inscription_id, owner_address) in &burn_events {
            try_info!(
                ctx,
                "DogeLotto burn: inscription_id={} owner={} (+1 Burn Point)",
                inscription_id,
                owner_address,
            );
        }

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
                    ticket.tip_percent,
                );
                webhooks::fire_webhooks(
                    webhook_urls.clone(),
                    config.webhooks.hmac_secret.clone(),
                    payload,
                );
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
                    winner.gross_payout_koinu,
                    winner.tip_percent,
                    winner.tip_deduction_koinu,
                    winner.payout_koinu,
                    &winner.seed_numbers,
                    &winner.drawn_numbers,
                );
                webhooks::fire_webhooks(
                    webhook_urls.clone(),
                    config.webhooks.hmac_secret.clone(),
                    payload,
                );
            }
        }

        if !lotto_deploy_map.is_empty()
            || !inserted_lotto_tickets.is_empty()
            || !resolved_lotto_winners.is_empty()
            || !burn_events.is_empty()
        {
            try_info!(
                ctx,
                "DogeLotto at block #{block_height}: {} deploy(s), {} ticket mint(s), {} winner record(s), {} burn(s)",
                lotto_deploy_map.len(),
                inserted_lotto_tickets.len(),
                resolved_lotto_winners.len(),
                burn_events.len(),
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
        checkpoint::write_checkpoint(config, block_height)?;
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
        doginals_pg::rollback_dogetags(block_height, &ord_tx).await?;
        doginals_pg::rollback_dogespells(block_height, &ord_tx).await?;
        doginals_pg::rollback_dmp_ops(block_height, &ord_tx).await?;
        doginals_pg::rollback_lotto_resolutions(block_height, &ord_tx).await?;
        doginals_pg::rollback_lotto_burns(block_height, &ord_tx).await?;
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

/// Detect lotto ticket burns for the "Burners" mechanic.
/// Returns a list of (inscription_id, owner_address) for all burned tickets.
async fn detect_lotto_burns<T: deadpool_postgres::GenericClient>(
    _block: &DogecoinBlockData,
    burn_address: &str,
    block_height: u64,
    block_timestamp: u32,
    client: &T,
    ctx: &Context,
) -> Result<Vec<(String, String)>, String> {
    let mut burned = Vec::new();

    // Query inscriptions transferred TO burn address in this block.
    // `updated_address` and `tx_id` live in `locations`, joined via
    // (ordinal_number, block_height, tx_index).
    let rows = client
        .query(
            "SELECT DISTINCT it.inscription_id, it.block_height, l.tx_id
             FROM inscription_transfers it
             JOIN lotto_tickets lt ON it.inscription_id = lt.inscription_id
             JOIN locations l ON l.ordinal_number = it.ordinal_number
                              AND l.block_height = it.block_height
                              AND l.tx_index = it.tx_index
             WHERE it.block_height::bigint = $1
               AND l.address = $2",
            &[&(block_height as i64), &burn_address],
        )
        .await
        .map_err(|e| format!("detect_lotto_burns (query transfers): {e}"))?;

    for row in rows {
        let inscription_id: String = row.get(0);
        let tx_id: String = row.get(2);

        // Get ticket info
        if let Some(ticket_info) =
            doginals_pg::get_lotto_ticket_by_inscription(&inscription_id, client).await?
        {
            // Get lottery to check if resolved/expired
            if let Some(lotto_full) =
                doginals_pg::get_lotto_lottery(&ticket_info.lotto_id, client).await?
            {
                // Only allow burning tickets from resolved or expired lotteries
                if lotto_full.summary.resolved || block_height > lotto_full.summary.draw_block {
                    // Get the previous owner (sender) from inscription_transfers
                    // This is the address that sent the inscription to burn_address
                    let owner_query = client
                        .query_opt(
                            "SELECT l.address
                             FROM inscription_transfers it
                             JOIN locations l ON l.ordinal_number = it.ordinal_number
                                              AND l.block_height = it.block_height
                                              AND l.tx_index = it.tx_index
                             WHERE it.inscription_id = $1
                               AND it.block_height::bigint < $2
                             ORDER BY it.block_height DESC, it.tx_index DESC
                             LIMIT 1",
                            &[&inscription_id, &(block_height as i64)],
                        )
                        .await
                        .map_err(|e| format!("detect_lotto_burns (get prev owner): {e}"))?;

                    let owner_address = if let Some(owner_row) = owner_query {
                        owner_row.get::<_, String>(0)
                    } else {
                        // Fallback: genesis address from inscriptions table
                        let genesis_query = client
                            .query_opt(
                                "SELECT address FROM inscriptions WHERE inscription_id = $1",
                                &[&inscription_id],
                            )
                            .await
                            .map_err(|e| format!("detect_lotto_burns (get genesis): {e}"))?;
                        if let Some(g) = genesis_query {
                            g.get(0)
                        } else {
                            continue; // Skip if we can't find owner
                        }
                    };

                    // Record the burn
                    doginals_pg::record_lotto_burn(
                        &inscription_id,
                        &ticket_info.lotto_id,
                        &ticket_info.ticket_id,
                        &owner_address,
                        block_height,
                        block_timestamp,
                        &tx_id,
                        client,
                    )
                    .await?;

                    burned.push((inscription_id.clone(), owner_address.clone()));

                    try_info!(
                        ctx,
                        "Burned lotto ticket {} ({}) from {} → +1 Burn Point",
                        inscription_id,
                        ticket_info.lotto_id,
                        owner_address
                    );
                }
            }
        }
    }

    Ok(burned)
}
