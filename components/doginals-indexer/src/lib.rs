use core::{
    first_inscription_height,
    meta_protocols::drc20::cache::drc20_new_cache,
    new_traversals_lazy_cache,
    pipeline::processors::{
        block_archiving::store_compacted_blocks,
        inscription_indexing::{process_blocks, rollback_block},
    },
    protocol::{
        inscription_parsing::parse_inscriptions_in_standardized_block,
        sequence_cursor::SequenceCursor,
    },
};
use std::{
    collections::HashMap,
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
};

use config::Config;
use db::{
    blocks::{self, find_last_block_inserted, open_blocks_db_with_retry},
    checkpoint, doginals_pg, migrate_dbs,
};
use deadpool_postgres::Pool;
use dogecoin::{
    start_dogecoin_indexer, try_debug, try_info,
    types::BlockIdentifier,
    utils::{future_block_on, Context},
    Indexer, IndexerCommand,
};
use lru::LruCache;
use postgres::{pg_pool, pg_pool_client};
use utils::monitoring::{start_serving_prometheus_metrics, PrometheusMonitoring};

#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate lazy_static;

extern crate serde;

pub mod cache;
pub mod core;
pub mod db;
pub mod manifest;
pub mod utils;

#[derive(Debug, Clone)]
pub struct PgConnectionPools {
    pub ordinals: Pool,
    pub drc20: Option<Pool>,
}

fn pg_pools(config: &Config) -> PgConnectionPools {
    PgConnectionPools {
        ordinals: pg_pool(&config.doginals.as_ref().unwrap().db).unwrap(),
        drc20: config
            .ordinals_drc20_config()
            .map(|drc20| pg_pool(&drc20.db).unwrap()),
    }
}

async fn new_ordinals_indexer_runloop(
    prometheus: &PrometheusMonitoring,
    abort_signal: &Arc<AtomicBool>,
    config: &Config,
    ctx: &Context,
) -> Result<Indexer, String> {
    manifest::init_manifest();
    let (commands_tx, commands_rx) =
        crossbeam_channel::bounded(config.resources.indexer_channel_capacity);
    let pg_pools = pg_pools(config);

    let config_moved = config.clone();
    let ctx_moved = ctx.clone();
    let pg_pools_moved = pg_pools.clone();
    let prometheus_moved = prometheus.clone();
    let abort_signal_moved = abort_signal.clone();
    let handle: JoinHandle<()> = hiro_system_kit::thread_named("ordinals_indexer")
        .spawn(move || {
            future_block_on(&ctx_moved.clone(), async move {
                let cache_l2 = Arc::new(new_traversals_lazy_cache(2048));
                let mut recent_blocks_cache = LruCache::new(std::num::NonZeroUsize::new(1_000).unwrap());
                let recent_owners_cache = Arc::new(dashmap::DashMap::<u64, String>::new());
                let garbage_collect_every_n_blocks = 100;
                let mut garbage_collect_nth_block = 0;

                let mut sequence_cursor = SequenceCursor::new();
                let mut drc20_cache: Option<core::meta_protocols::drc20::cache::Brc20MemoryCache> =
                    drc20_new_cache(&config_moved);
                loop {
                    if abort_signal_moved.load(Ordering::SeqCst) {
                        break;
                    }
                    match commands_rx.recv() {
                        Ok(command) => match command {
                            IndexerCommand::StoreCompactedBlocks(blocks) => {
                                let blocks_db_rw =
                                    open_blocks_db_with_retry(true, &config_moved, &ctx_moved);
                                store_compacted_blocks(blocks, true, &blocks_db_rw, &ctx_moved);
                            }
                            IndexerCommand::IndexBlocks {
                                mut apply_blocks,
                                rollback_block_ids,
                            } => {
                                if !rollback_block_ids.is_empty() {
                                    let blocks_db_rw =
                                        open_blocks_db_with_retry(true, &config_moved, &ctx_moved);
                                    for block_id in rollback_block_ids.iter() {
                                        blocks::delete_blocks_in_block_range(
                                            block_id.index as u32,
                                            block_id.index as u32,
                                            &blocks_db_rw,
                                            &ctx_moved,
                                        );
                                        rollback_block(
                                            block_id.index,
                                            &config_moved,
                                            &pg_pools_moved,
                                            &ctx_moved,
                                        )
                                        .await?;
                                    }
                                    blocks_db_rw.flush().map_err(|e| {
                                        format!("error dropping rollback blocks from rocksdb: {e}")
                                    })?;
                                }

                                let blocks = match process_blocks(
                                    &mut apply_blocks,
                                    &mut sequence_cursor,
                                    &cache_l2,
                                    &mut drc20_cache,
                                    &prometheus_moved,
                                    &config_moved,
                                    &pg_pools_moved,
                                    &ctx_moved,
                                    &abort_signal_moved,
                                )
                                .await
                                {
                                    Ok(blocks) => blocks,
                                    Err(e) => return Err(format!("error indexing blocks: {e}")),
                                };


                                for block in &blocks {
                                    recent_blocks_cache.put(block.block_identifier.index, block.block_identifier.hash.clone());
                                    for tx in &block.transactions {
                                        for op in &tx.metadata.ordinal_operations {
                                            if let dogecoin::types::OrdinalOperation::InscriptionTransferred(data) = op {
                                                recent_owners_cache.insert(data.ordinal_number, format!("{:?}", data.destination));
                                            }
                                        }
                                    }
                                }
                                garbage_collect_nth_block += blocks.len();
                                if garbage_collect_nth_block > garbage_collect_every_n_blocks {
                                    try_debug!(
                                        ctx_moved,
                                        "Clearing cache L2 ({} entries)",
                                        cache_l2.len()
                                    );
                                    cache_l2.clear();
                                    garbage_collect_nth_block = 0;
                                }
                            }
                            IndexerCommand::Terminate => {
                                break;
                            }
                        },
                        Err(e) => return Err(format!("ordinals indexer channel error: {e}")),
                    }
                }
                try_info!(ctx_moved, "DoginalIndexer thread complete");
                Ok(())
            });
        })
        .expect("unable to spawn thread");

    let pg_chain_tip = {
        let ord_client = pg_pool_client(&pg_pools.ordinals).await?;
        db::doginals_pg::get_chain_tip(&ord_client).await?
    };
    let blocks_chain_tip = {
        let blocks_db = open_blocks_db_with_retry(false, config, ctx);
        let height = find_last_block_inserted(&blocks_db);
        // Blocks DB does not have the hash available.
        if height > 0 {
            Some(BlockIdentifier {
                index: height as u64,
                hash: "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
            })
        } else {
            None
        }
    };
    let checkpoint_tip = checkpoint::read_checkpoint(config)?.map(|index| BlockIdentifier {
        index,
        hash: "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
    });

    let chain_tip = match (pg_chain_tip, blocks_chain_tip, checkpoint_tip) {
        (Some(x), Some(y), Some(z)) => Some([x, y, z].into_iter().min_by_key(|b| b.index).unwrap()),
        (Some(x), Some(y), None) => Some(if x.index <= y.index { x } else { y }),
        (Some(x), None, Some(z)) => Some(if x.index <= z.index { x } else { z }),
        (None, Some(y), Some(z)) => Some(if y.index <= z.index { y } else { z }),
        (Some(_), None, None) => None,
        (None, Some(y), None) | (None, None, Some(y)) => {
            let x = BlockIdentifier {
                index: first_inscription_height(config) - 1,
                hash: "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
            };
            Some(if x.index <= y.index { x } else { y })
        }
        (None, None, None) => None,
    };
    Ok(Indexer {
        commands_tx,
        chain_tip,
        thread_handle: Some(handle),
        file_blocks_synced: 0,
        rpc_blocks_synced: 0,
    })
}

// Re-export row types so callers (CLI) don't need to reach into db internals.
pub use db::doginals_pg::{
    BurnPointsRow, DmpListingRow, DnsNameRow, DogemapClaimRow, LottoStatusRow, LottoSummaryRow,
    LottoTicketCardRow, LottoTicketInfoRow, LottoWinnerRow,
};

/// List active DMP listings.
pub async fn dmp_list_listings(
    limit: usize,
    offset: usize,
    config: &Config,
) -> Result<Vec<DmpListingRow>, String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    db::doginals_pg::list_dmp_listings(limit, offset, &client).await
}

/// Look up a single DNS name registration.
pub async fn dns_resolve(name: &str, config: &Config) -> Result<Option<DnsNameRow>, String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    db::doginals_pg::get_dns_name(name, &client).await
}

/// List DNS name registrations, optionally filtered by namespace.
pub async fn dns_list(
    namespace: Option<&str>,
    limit: usize,
    offset: usize,
    config: &Config,
) -> Result<(Vec<DnsNameRow>, i64), String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    let rows = db::doginals_pg::list_dns_names(namespace, limit, offset, &client).await?;
    let total = db::doginals_pg::count_dns_names(&client).await?;
    Ok((rows, total))
}

/// Look up a single Dogemap claim by block number.
pub async fn dogemap_status(
    block_number: u32,
    config: &Config,
) -> Result<Option<DogemapClaimRow>, String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    db::doginals_pg::get_dogemap_claim(block_number, &client).await
}

/// List Dogemap claims.
pub async fn dogemap_list(
    limit: usize,
    offset: usize,
    config: &Config,
) -> Result<(Vec<DogemapClaimRow>, i64), String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    let rows = db::doginals_pg::list_dogemap_claims(limit, offset, &client).await?;
    let total = db::doginals_pg::count_dogemap_claims(&client).await?;
    Ok((rows, total))
}

/// Look up a single DogeLotto deployment and any resolved winners.
pub async fn lotto_status(
    lotto_id: &str,
    config: &Config,
) -> Result<Option<LottoStatusRow>, String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    db::doginals_pg::get_lotto_lottery(lotto_id, &client).await
}

/// List DogeLotto deployments.
pub async fn lotto_list(
    limit: usize,
    offset: usize,
    config: &Config,
) -> Result<(Vec<LottoSummaryRow>, i64), String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    let rows = db::doginals_pg::list_lotto_lotteries(limit, offset, &client).await?;
    let total = db::doginals_pg::count_lotto_lotteries(&client).await?;
    Ok((rows, total))
}

/// Get lotto ticket info by inscription ID (for burn detection).

/// List lotto tickets for a specific lotto deployment.
pub async fn lotto_list_tickets(
    lotto_id: &str,
    limit: usize,
    offset: usize,
    config: &Config,
) -> Result<Vec<LottoTicketCardRow>, String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    db::doginals_pg::list_lotto_tickets(lotto_id, limit, offset, &client).await
}
pub async fn lotto_get_ticket_info(
    inscription_id: &str,
    config: &Config,
) -> Result<Option<LottoTicketInfoRow>, String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    db::doginals_pg::get_lotto_ticket_by_inscription(inscription_id, &client).await
}

/// Get burn points for a specific address.
pub async fn lotto_get_burn_points(
    owner_address: &str,
    config: &Config,
) -> Result<Option<BurnPointsRow>, String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    db::doginals_pg::get_burn_points(owner_address, &client).await
}

/// Get top burners leaderboard.
pub async fn lotto_get_top_burners(
    limit: usize,
    config: &Config,
) -> Result<Vec<BurnPointsRow>, String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let client = pg_pool_client(&pool).await?;
    db::doginals_pg::get_top_burners(limit, &client).await
}

pub async fn get_chain_tip(config: &Config) -> Result<BlockIdentifier, String> {
    let pool = pg_pool(&config.doginals.as_ref().unwrap().db)?;
    let ord_client = pg_pool_client(&pool).await?;
    Ok(db::doginals_pg::get_chain_tip(&ord_client)
        .await?
        .unwrap_or(BlockIdentifier {
            index: first_inscription_height(config) - 1,
            hash: "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
        }))
}

pub async fn rollback_block_range(
    start_block: u64,
    end_block: u64,
    config: &Config,
    ctx: &Context,
) -> Result<(), String> {
    let blocks_db_rw = open_blocks_db_with_retry(true, config, ctx);
    let pg_pools = pg_pools(config);
    blocks::delete_blocks_in_block_range(start_block as u32, end_block as u32, &blocks_db_rw, ctx);
    for block in start_block..=end_block {
        rollback_block(block, config, &pg_pools, ctx).await?;
    }
    blocks_db_rw
        .flush()
        .map_err(|e| format!("error dropping rollback blocks from rocksdb: {e}"))
}

/// Starts the ordinals indexing process. Will block the main thread indefinitely until explicitly stopped or it reaches chain tip
/// and `stream_blocks_at_chain_tip` is set to false.
pub async fn start_doginals_indexer(
    stream_blocks_at_chain_tip: bool,
    abort_signal: &Arc<AtomicBool>,
    config: &Config,
    ctx: &Context,
) -> Result<(), String> {
    migrate_dbs(config, ctx).await?;
    let prometheus = PrometheusMonitoring::new();
    let pg_pools = pg_pools(config);

    // Initialize metrics with current state
    let max_inscription_number = {
        let ord_client = pg_pool_client(&pg_pools.ordinals).await?;
        doginals_pg::get_highest_inscription_number(&ord_client)
            .await?
            .unwrap_or(0) as u64
    };
    let chain_tip = get_chain_tip(config).await?;
    prometheus
        .initialize(max_inscription_number, chain_tip.index, &pg_pools)
        .await?;

    let mut indexer = new_ordinals_indexer_runloop(&prometheus, abort_signal, config, ctx).await?;

    if let Some(metrics) = &config.metrics {
        if metrics.enabled {
            let registry_moved = prometheus.registry.clone();
            let ctx_cloned = ctx.clone();
            let port = metrics.prometheus_port;
            let abort_signal_cloned = abort_signal.clone();
            let _ = std::thread::spawn(move || {
                hiro_system_kit::nestable_block_on(start_serving_prometheus_metrics(
                    port,
                    registry_moved,
                    ctx_cloned,
                    abort_signal_cloned,
                ));
            });
        }
    }

    start_dogecoin_indexer(
        &mut indexer,
        first_inscription_height(config),
        stream_blocks_at_chain_tip,
        true,
        abort_signal,
        config,
        ctx,
    )
    .await?;

    // Report how many blocks were synced via each path to Prometheus.
    if indexer.file_blocks_synced > 0 {
        prometheus.metrics_add_file_blocks(indexer.file_blocks_synced);
        try_info!(
            ctx,
            "Session summary: {} blocks via .blk file, {} blocks via RPC",
            indexer.file_blocks_synced,
            indexer.rpc_blocks_synced
        );
    }
    if indexer.rpc_blocks_synced > 0 {
        prometheus.metrics_add_rpc_blocks(indexer.rpc_blocks_synced);
    }

    Ok(())
}

/// Options controlling what `scan_doginals` emits.
pub struct ScanOptions {
    /// Only emit `inscription_revealed` entries (skip transfers).
    pub reveals_only: bool,
    /// If set, only emit inscriptions whose content_type starts with this prefix.
    pub content_type_prefix: Option<String>,
}

/// Scan a block range for inscriptions without writing anything to Postgres.
///
/// For each block in `from_block..=to_block` the function:
/// 1. Fetches the block via the .blk pipeline (if available) or RPC.
/// 2. Parses inscriptions using the same logic as the full indexer.
/// 3. Writes one JSON object per inscription to `writer` (JSONL format).
///
/// Returns the number of inscriptions found.
pub async fn scan_doginals<W: Write + Send + 'static>(
    from_block: u64,
    to_block: u64,
    opts: ScanOptions,
    writer: Arc<std::sync::Mutex<W>>,
    abort_signal: &Arc<AtomicBool>,
    config: &Config,
    ctx: &Context,
) -> Result<u64, String> {
    use crossbeam_channel::bounded;
    use dogecoin::{types::OrdinalOperation, IndexerCommand};

    if from_block > to_block {
        return Err(format!(
            "--from ({from_block}) must be <= --to ({to_block})"
        ));
    }

    // Build a scan config: same as the caller's config but with the exact range.
    let mut scan_config = config.clone();
    scan_config.start_block = Some(from_block);
    scan_config.stop_block = Some(to_block);

    // Counter shared between the handler thread and this function.
    let found = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let found_moved = found.clone();

    // Channel for blocks coming in from the dogecoin pipeline.
    let (commands_tx, commands_rx) =
        bounded::<IndexerCommand>(config.resources.indexer_channel_capacity);

    let config_moved = config.clone();
    let ctx_moved = ctx.clone();
    let abort_signal_moved = abort_signal.clone();
    let opts_reveals_only = opts.reveals_only;
    let opts_content_type = opts.content_type_prefix.clone();
    let writer_moved = writer.clone();

    let handle: JoinHandle<()> = hiro_system_kit::thread_named("scan_handler")
        .spawn(move || {
            // future_block_on needs &Context; keep a clone for that reference while
            // ctx_moved is captured by move into the async block.
            let ctx_ref = ctx_moved.clone();
            dogecoin::utils::future_block_on(&ctx_ref, async move {
                loop {
                    if abort_signal_moved.load(Ordering::SeqCst) {
                        break;
                    }
                    match commands_rx.recv() {
                        Ok(cmd) => match cmd {
                            IndexerCommand::StoreCompactedBlocks(_) => {
                                // Not needed for scan — skip.
                            }
                            IndexerCommand::IndexBlocks { mut apply_blocks, .. } => {
                                for block in apply_blocks.iter_mut() {
                                    let mut drc20_map = HashMap::new();
                                    let mut dns_map = HashMap::new();
                                    let mut dogemap_map = HashMap::new();
                                    let mut lotto_deploy_map = HashMap::new();
                                    let mut lotto_mints = vec![];
                                    let mut dmp_ops = vec![];
                                    parse_inscriptions_in_standardized_block(
                                        block,
                                        &mut drc20_map,
                                        &mut dns_map,
                                        &mut dogemap_map,
                                        &mut lotto_deploy_map,
                                        &mut lotto_mints,
                                        &mut dmp_ops,
                                        &config_moved,
                                        &ctx_moved,
                                    );
                                    for tx in &block.transactions {
                                        for op in &tx.metadata.ordinal_operations {
                                            match op {
                                                OrdinalOperation::InscriptionRevealed(reveal) => {
                                                    if let Some(ref prefix) = opts_content_type {
                                                        if !reveal.content_type.starts_with(prefix.as_str()) {
                                                            continue;
                                                        }
                                                    }
                                                    found_moved.fetch_add(1, Ordering::Relaxed);
                                                    let line = serde_json::json!({
                                                        "type": "inscription_revealed",
                                                        "block_height": block.block_identifier.index,
                                                        "block_hash": block.block_identifier.hash,
                                                        "tx_id": tx.transaction_identifier.hash,
                                                        "inscription_id": reveal.inscription_id,
                                                        "content_type": reveal.content_type,
                                                        "content_length": reveal.content_length,
                                                        "content_bytes": reveal.content_bytes,
                                                        "inscriber_address": reveal.inscriber_address,
                                                        "curse_type": reveal.curse_type,
                                                        "metaprotocol": reveal.metaprotocol,
                                                        "metadata": reveal.metadata,
                                                        "parents": reveal.parents,
                                                        "delegate": reveal.delegate,
                                                    });
                                                    let mut w = writer_moved.lock().unwrap();
                                                    let _ = writeln!(w, "{}", line);
                                                }
                                                OrdinalOperation::InscriptionTransferred(transfer) => {
                                                    if opts_reveals_only {
                                                        continue;
                                                    }
                                                    found_moved.fetch_add(1, Ordering::Relaxed);
                                                    let line = serde_json::json!({
                                                        "type": "inscription_transferred",
                                                        "block_height": block.block_identifier.index,
                                                        "block_hash": block.block_identifier.hash,
                                                        "tx_id": tx.transaction_identifier.hash,
                                                        "ordinal_number": transfer.ordinal_number,
                                                        "destination": transfer.destination,
                                                    });
                                                    let mut w = writer_moved.lock().unwrap();
                                                    let _ = writeln!(w, "{}", line);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            IndexerCommand::Terminate => break,
                        },
                        Err(_) => break,
                    }
                }
                Ok::<(), String>(())
            });
        })
        .expect("unable to spawn scan_handler thread");

    // Determine which chain tip to report as our current index tip so the
    // dogecoin pipeline knows where to start downloading from.
    let chain_tip = if from_block == 0 {
        None
    } else {
        Some(dogecoin::types::BlockIdentifier {
            index: from_block - 1,
            hash: "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
        })
    };

    let mut indexer = dogecoin::Indexer {
        commands_tx,
        chain_tip,
        thread_handle: Some(handle),
        file_blocks_synced: 0,
        rpc_blocks_synced: 0,
    };

    start_dogecoin_indexer(
        &mut indexer,
        first_inscription_height(config),
        false,
        false,
        abort_signal,
        &scan_config,
        ctx,
    )
    .await?;

    Ok(found.load(Ordering::Relaxed))
}
