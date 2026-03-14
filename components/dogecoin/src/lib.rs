// The try_warn!/try_info! macros use a trailing semicolon inside their bodies.
// Suppress the future-compatibility warning until the upstream macros are fixed.
#![allow(semicolon_in_expressions_from_macros)]

extern crate serde;

#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate serde_json;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};

pub use bitcoincore_rpc;
use config::{Config, DogecoinDataSource};

use crate::{
    blk_reader::BlkReader,
    block_pool::BlockPool,
    pipeline::{
        blk::start_file_block_download_pipeline,
        block_processor_runloop, download_rpc_blocks,
        rpc::{build_http_client, retrieve_block_hash_with_retry},
        wait_for_thread_finish, BlockProcessor, BlockProcessorCommand,
    },
    types::{BlockIdentifier, DogecoinBlockData, DogecoinNetwork},
    utils::{
        dogecoind::{dogecoin_get_chain_tip, dogecoin_wait_for_chain_tip},
        future_block_on, Context,
    },
};

pub mod blk_reader;
pub mod block_pool;
pub mod chainparams;
pub mod network_params;
pub mod pipeline;
pub mod types;
pub mod utils;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Commands that can be sent to the indexer.
pub enum IndexerCommand {
    /// Store compacted blocks.
    StoreCompactedBlocks(Vec<(u64, Vec<u8>)>),
    /// Index standardized blocks into the indexer's database.
    IndexBlocks {
        apply_blocks: Vec<DogecoinBlockData>,
        rollback_block_ids: Vec<BlockIdentifier>,
    },
    /// Terminate the indexer gracefully.
    Terminate,
}

/// Object that will receive standardized Dogecoin blocks ready to be indexed or rolled back.
/// Blocks can come from historical downloads or recent block streams.
pub struct Indexer {
    /// Sender for emitting indexer commands.
    pub commands_tx: crossbeam_channel::Sender<IndexerCommand>,
    /// Current index chain tip at launch time.
    pub chain_tip: Option<BlockIdentifier>,
    /// Handle for the indexer thread.
    pub thread_handle: Option<JoinHandle<()>>,
    /// Number of blocks processed via direct `.blk` file reads in the last sync.
    pub file_blocks_synced: u64,
    /// Number of blocks processed via JSON-RPC in the last sync.
    pub rpc_blocks_synced: u64,
}

/// Starts a Dogecoin block indexer pipeline.
#[cfg_attr(not(feature = "zeromq"), allow(unused_variables))]
pub async fn start_dogecoin_indexer(
    indexer: &mut Indexer,
    sequence_start_block_height: u64,
    stream_blocks_at_chain_tip: bool,
    compress_blocks: bool,
    abort_signal: &Arc<AtomicBool>,
    config: &Config,
    ctx: &Context,
) -> Result<(), String> {
    let mut dogecoin_chain_tip = dogecoin_wait_for_chain_tip(&config.dogecoin, ctx);
    let http_client = build_http_client();

    // Block pool that will track the canonical chain and detect any reorgs that may happen.
    // Dogecoin's 1-minute blocks and higher reorg frequency make this especially important.
    let block_pool_arc = Arc::new(Mutex::new(BlockPool::new()));
    let block_pool = block_pool_arc.clone();
    // Block cache that will keep block data in memory while it is prepared to be sent to indexers.
    let block_store_arc = Arc::new(Mutex::new(HashMap::new()));

    // -----------------------------------------------------------------------
    // Decide on data source based on config.dogecoin.data_source.
    // Auto:  Try .blk files; fall back to RPC if unavailable.
    // File:  Require .blk files; abort if index cannot be opened.
    // Rpc:   Skip .blk files entirely.
    // -----------------------------------------------------------------------
    let index_copy_dir = config.effective_blk_index_copy_dir().ok_or_else(|| {
        "unable to determine Dogecoin blk-index copy dir; set DOGECOIN_DATA_DIR \
         or dogecoin.dogecoin_data_dir in config"
            .to_string()
    })?;

    let blk_reader: Option<BlkReader> = match config.dogecoin.data_source {
        DogecoinDataSource::Rpc => {
            try_info!(ctx, "Data source: RPC (direct .blk reads disabled)");
            None
        }
        DogecoinDataSource::File => {
            let blocks_dir = config.effective_dogecoin_blocks_dir().ok_or_else(|| {
                "data_source = \"file\" requires a Dogecoin Core data dir. \
                 Set DOGECOIN_DATA_DIR or dogecoin.dogecoin_data_dir."
                    .to_string()
            })?;
            match BlkReader::open(&blocks_dir, &index_copy_dir, ctx) {
                Some(r) => {
                    try_info!(
                        ctx,
                        "Data source: FILE — using direct .blk reads (5-20× faster initial sync). \
                         Max height in index: {}",
                        r.max_height()
                    );
                    Some(r)
                }
                None => {
                    return Err(
                        "data_source = \"file\" but the .blk index could not be opened. \
                         Run `kabosu doginals index refresh-blk-index` first, or \
                         set data_source = \"auto\" to fall back to RPC."
                            .to_string(),
                    );
                }
            }
        }
        DogecoinDataSource::Auto => match config.effective_dogecoin_blocks_dir() {
            None => {
                try_info!(
                    ctx,
                    "Data source: RPC (set DOGECOIN_DATA_DIR for 5-20× faster sync)"
                );
                None
            }
            Some(blocks_dir) => match BlkReader::open(&blocks_dir, &index_copy_dir, ctx) {
                Some(r) => {
                    try_info!(
                        ctx,
                        "Data source: FILE — using direct .blk reads (5-20× faster \
                             initial sync). Max height in index: {}",
                        r.max_height()
                    );
                    Some(r)
                }
                None => {
                    try_info!(
                        ctx,
                        "Data source: RPC (falling back — .blk index unavailable). \
                             Run `kabosu doginals index refresh-blk-index` to enable \
                             fast mode on next start."
                    );
                    None
                }
            },
        },
    };

    if let Some(index_chain_tip) = indexer.chain_tip.as_mut() {
        if !index_chain_tip.has_known_hash() {
            if let Some(reader) = blk_reader.as_ref() {
                if let Some(hash) = reader.hash_at_height(index_chain_tip.index as u32) {
                    index_chain_tip.hash = hash;
                    try_info!(
                        ctx,
                        "Resolved index chain tip hash from .blk index at height #{}",
                        index_chain_tip.index
                    );
                }
            }

            if !index_chain_tip.has_known_hash() {
                match retrieve_block_hash_with_retry(
                    &http_client,
                    &index_chain_tip.index,
                    &config.dogecoin,
                    ctx,
                )
                .await
                {
                    Ok(hash) => {
                        index_chain_tip.hash = format!("0x{}", hash);
                        try_info!(
                            ctx,
                            "Resolved index chain tip hash from RPC at height #{}",
                            index_chain_tip.index
                        );
                    }
                    Err(e) => {
                        try_info!(
                            ctx,
                            "Index chain tip hash unavailable at height #{} ({e}); \
                             continuing without fork-pool priming",
                            index_chain_tip.index
                        );
                    }
                }
            }
        }
    }

    if let Some(index_chain_tip) = &indexer.chain_tip {
        try_info!(ctx, "Index chain tip is at {}", index_chain_tip);
    } else {
        try_info!(ctx, "Index is empty");
    }

    // Build the [BlockProcessor] that will be used to ingest and standardize blocks from the
    // Dogecoin node. This processor will then send blocks to the [Indexer] for indexing.
    let (commands_tx, commands_rx) = crossbeam_channel::bounded::<BlockProcessorCommand>(
        config.resources.indexer_channel_capacity,
    );
    let ctx_moved = ctx.clone();
    let config_moved = config.clone();
    let block_pool_moved = block_pool.clone();
    let block_store_moved = block_store_arc.clone();
    let http_client_moved = http_client.clone();
    let indexer_commands_tx_moved = indexer.commands_tx.clone();
    let index_chain_tip_moved = indexer.chain_tip.clone();
    let abort_signal_moved = abort_signal.clone();
    let handle: JoinHandle<()> = hiro_system_kit::thread_named("block_download_processor")
        .spawn(move || {
            future_block_on(&ctx_moved.clone(), async move {
                block_processor_runloop(
                    &indexer_commands_tx_moved,
                    &index_chain_tip_moved,
                    &commands_rx,
                    &block_pool_moved,
                    &block_store_moved,
                    &http_client_moved,
                    sequence_start_block_height,
                    &abort_signal_moved,
                    &config_moved,
                    &ctx_moved,
                )
                .await
            });
        })
        .expect("unable to spawn thread");
    let mut block_processor = BlockProcessor {
        commands_tx: commands_tx.clone(),
        thread_handle: Some(handle),
    };

    // -----------------------------------------------------------------------
    // Phase 1 (optional): Fast historical sync via direct .blk file reads.
    // Covers blocks from the current index tip up to blk_reader.max_height()
    // (further capped by config.stop_block for test/debug ranges).
    // The BlockProcessor keeps running after this phase so Phase 2 can follow.
    // -----------------------------------------------------------------------
    if let Some(ref reader) = blk_reader {
        if !abort_signal.load(Ordering::SeqCst) {
            let blk_max = reader.max_height() as u64;
            let current_tip = indexer.chain_tip.as_ref().map(|t| t.index).unwrap_or(0);
            let file_start = current_tip
                .max(sequence_start_block_height.saturating_sub(1))
                .saturating_add(if current_tip == 0 { 0 } else { 1 });
            // Cap to the node's current tip, the blk index max, and any explicit stop_block.
            let mut file_end = blk_max.min(dogecoin_chain_tip.index);
            if let Some(stop) = config.stop_block {
                file_end = file_end.min(stop);
            }

            if file_start <= file_end {
                let heights: Vec<u64> = (file_start..=file_end).collect();
                let block_count = heights.len() as u64;
                let network = DogecoinNetwork::from_network(config.dogecoin.network);
                try_info!(
                    ctx,
                    "Phase 1 (.blk): syncing #{} → #{} ({} blocks)",
                    file_start,
                    file_end,
                    block_count
                );
                start_file_block_download_pipeline(
                    reader,
                    heights,
                    sequence_start_block_height,
                    compress_blocks,
                    &commands_tx,
                    abort_signal,
                    &network,
                    ctx,
                )
                .await?;
                indexer.file_blocks_synced = block_count;
                try_info!(
                    ctx,
                    "Phase 1 (.blk) complete: {} blocks synced at up to {} blk/s",
                    block_count,
                    block_count // actual bps is logged inside the pipeline
                );
            } else {
                try_info!(
                    ctx,
                    "Phase 1 (.blk): index already up-to-date through #{file_end}, skipping"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Phase 2: RPC sync loop — catches up from current tip to chain tip.
    // Handles blocks above blk_reader.max_height() (or all blocks in Rpc mode).
    // Skipped when stop_block was set and already reached in Phase 1.
    // -----------------------------------------------------------------------
    let skip_rpc = config.stop_block.is_some();
    if skip_rpc {
        try_info!(
            ctx,
            "Phase 2 (RPC): skipped (stop_block reached in Phase 1)"
        );
    } else {
        try_info!(ctx, "Phase 2 (RPC): syncing remaining blocks to chain tip");
    }

    let rpc_start_tip = indexer.chain_tip.as_ref().map(|t| t.index).unwrap_or(0);

    if !skip_rpc {
        loop {
            if abort_signal.load(Ordering::SeqCst) {
                break;
            }
            {
                let pool = block_pool.lock().unwrap();
                let chain_tip = pool.canonical_chain_tip().or(indexer.chain_tip.as_ref());
                if let Some(chain_tip) = chain_tip {
                    if dogecoin_chain_tip == *chain_tip {
                        try_info!(
                            ctx,
                            "Phase 2 (RPC): reached chain tip at {dogecoin_chain_tip}"
                        );
                        break;
                    }
                }
            }
            download_rpc_blocks(
                indexer,
                &mut block_processor,
                &block_pool_arc,
                &http_client,
                dogecoin_chain_tip.index,
                sequence_start_block_height,
                compress_blocks,
                abort_signal,
                config,
                ctx,
            )
            .await?;
            // Dogecoin node may have advanced while we were indexing — re-check chain tip.
            dogecoin_chain_tip = dogecoin_get_chain_tip(&config.dogecoin, ctx);
        }
    }

    // Record how many blocks were synced via RPC this session.
    let rpc_end_tip = {
        let pool = block_pool.lock().unwrap();
        pool.canonical_chain_tip()
            .map(|t| t.index)
            .unwrap_or(rpc_start_tip)
    };
    indexer.rpc_blocks_synced = rpc_end_tip.saturating_sub(rpc_start_tip);

    // Stream new incoming blocks from the Dogecoin node's ZeroMQ interface (optional feature).
    #[cfg(feature = "zeromq")]
    if stream_blocks_at_chain_tip && !abort_signal.load(Ordering::SeqCst) {
        crate::pipeline::stream_zmq_blocks(
            &mut block_processor,
            sequence_start_block_height,
            compress_blocks,
            abort_signal,
            config,
            ctx,
        )
        .await?;
    }

    // Send a terminate command to the indexer and wait for it to finish.
    let _ = indexer.commands_tx.send(IndexerCommand::Terminate);
    wait_for_thread_finish(&mut indexer.thread_handle)?;

    Ok(())
}
