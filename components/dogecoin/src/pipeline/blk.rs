//! File-based block download pipeline that reads directly from `.blk` files.
//!
//! A faster alternative to the RPC pipeline for historical sync (5-20× speedup
//! on SSD). Spawns parallel reader threads and a dispatcher that re-orders
//! results before forwarding `ProcessBlocks` commands to the BlockProcessor.
//!
//! Unlike the RPC pipeline, this function does **not** send `Terminate` to the
//! BlockProcessor — the caller continues using the processor after this returns
//! (e.g. the RPC loop handles blocks above `blk_reader.max_height()`).

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::sleep,
    time::{Duration, Instant},
};

use crossbeam_channel::{bounded, Sender};

use crate::{
    blk_reader::BlkReader,
    pipeline::BlockProcessorCommand,
    try_info,
    types::{BlockBytesCursor, DogecoinBlockData, DogecoinNetwork},
    utils::Context,
};

/// Number of parallel `.blk` file reader threads.
const FILE_READER_THREADS: usize = 8;

/// Reads historical blocks from `.blk` files and pushes them to the
/// BlockProcessor via `commands_tx`.
///
/// Does **not** send `BlockProcessorCommand::Terminate`; the caller is
/// responsible for doing that when all downloading (file + RPC) is complete.
///
/// **Prevout limitation**: `previous_output.value` and
/// `previous_output.block_height` are set to 0 because `.blk` files don't
/// contain UTXO data. This is safe for inscription-content indexing but means
/// fee calculation and sat-range tracking use dummy values.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn start_file_block_download_pipeline(
    blk_reader: &BlkReader,
    block_heights: Vec<u64>,
    start_sequencing_blocks_at_height: u64,
    compress_blocks: bool,
    commands_tx: &Sender<BlockProcessorCommand>,
    abort_signal: &Arc<AtomicBool>,
    network: &DogecoinNetwork,
    ctx: &Context,
) -> Result<(), String> {
    if block_heights.is_empty() {
        return Ok(());
    }

    let first_height = *block_heights.first().unwrap();
    let last_height = *block_heights.last().unwrap();
    let total = block_heights.len() as u64;
    let t0 = Instant::now();

    try_info!(
        ctx,
        "BlkPipeline: reading {} blocks (#{} → #{}) from .blk files ({} threads)",
        total,
        first_height,
        last_height,
        FILE_READER_THREADS
    );

    let index = blk_reader.index_arc();
    let blocks_dir = blk_reader.blocks_dir().to_owned();
    let channel_cap = (total as usize).min(8_000);

    // Work queue: send heights to reader threads
    let (work_tx, work_rx) = bounded::<Option<u64>>(channel_cap);
    for &h in &block_heights {
        let _ = work_tx.send(Some(h));
    }
    for _ in 0..FILE_READER_THREADS {
        let _ = work_tx.send(None); // poison pills
    }

    // Reader → Dispatcher: (height, block_opt, compacted_opt)
    let (result_tx, result_rx) =
        bounded::<(u64, Option<DogecoinBlockData>, Option<Vec<u8>>)>(channel_cap);

    // -----------------------------------------------------------------------
    // Spawn reader threads
    // -----------------------------------------------------------------------
    let mut reader_handles = Vec::with_capacity(FILE_READER_THREADS);
    for thread_idx in 0..FILE_READER_THREADS {
        let work_rx = work_rx.clone();
        let result_tx = result_tx.clone();
        let index = index.clone();
        let blocks_dir = blocks_dir.clone();
        let network = network.clone();
        let abort = abort_signal.clone();
        let seq_start = start_sequencing_blocks_at_height;
        let do_compress = compress_blocks;
        let ctx_w = ctx.clone();

        let handle = hiro_system_kit::thread_named(&format!("BlkReader[{thread_idx}]"))
            .spawn(move || {
                loop {
                    if abort.load(Ordering::SeqCst) {
                        break;
                    }
                    let height = match work_rx.recv() {
                        Ok(None) | Err(_) => break,
                        Ok(Some(h)) => h,
                    };

                    use crate::blk_reader::read_block_by_height;
                    match read_block_by_height(&blocks_dir, &index, height as u32, &network) {
                        Ok(Some(block)) => {
                            let compacted = if do_compress {
                                BlockBytesCursor::from_standardized_block(&block).ok()
                            } else {
                                None
                            };
                            let block_opt = if height >= seq_start { Some(block) } else { None };
                            let _ = result_tx.send((height, block_opt, compacted));
                        }
                        Ok(None) => {
                            try_info!(
                                ctx_w,
                                "BlkReader[{thread_idx}]: #{height} not in .blk index"
                            );
                            let _ = result_tx.send((height, None, None));
                        }
                        Err(e) => {
                            try_info!(
                                ctx_w,
                                "BlkReader[{thread_idx}]: error at #{height}: {e}"
                            );
                            let _ = result_tx.send((height, None, None));
                        }
                    }
                }
            })
            .expect("unable to spawn BlkReader thread");
        reader_handles.push(handle);
    }
    drop(result_tx); // channel closes when all workers finish

    // -----------------------------------------------------------------------
    // Dispatcher: re-order by height and send ProcessBlocks to BlockProcessor
    // -----------------------------------------------------------------------
    let bp_tx = commands_tx.clone();
    let abort_d = abort_signal.clone();
    let ctx_d = ctx.clone();
    let seq_start = start_sequencing_blocks_at_height;

    let dispatcher = hiro_system_kit::thread_named("BlkDispatcher")
        .spawn(move || {
            let mut inbox: HashMap<u64, (Option<DogecoinBlockData>, Option<Vec<u8>>)> =
                HashMap::new();
            let mut cursor = seq_start.max(first_height);
            let mut received = 0u64;
            let mut forwarded = 0u64;

            'outer: loop {
                if abort_d.load(Ordering::SeqCst) {
                    break;
                }

                // Drain newly arrived results
                let mut got_any = false;
                loop {
                    match result_rx.try_recv() {
                        Ok((h, block_opt, compacted_opt)) => {
                            inbox.insert(h, (block_opt, compacted_opt));
                            received += 1;
                            got_any = true;
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            // All readers finished — force drain
                            received = total;
                            break;
                        }
                    }
                }

                // Send all consecutive in-order blocks to BlockProcessor
                let mut compacted_batch: Vec<(u64, Vec<u8>)> = vec![];
                let mut blocks_batch: Vec<DogecoinBlockData> = vec![];

                while let Some((block_opt, compacted_opt)) = inbox.remove(&cursor) {
                    if let Some(bytes) = compacted_opt {
                        compacted_batch.push((cursor, bytes));
                    }
                    if let Some(block) = block_opt {
                        blocks_batch.push(block);
                    }
                    forwarded += 1;
                    cursor += 1;
                }

                let has_data = !compacted_batch.is_empty() || !blocks_batch.is_empty();
                if has_data {
                    let _ = bp_tx.send(BlockProcessorCommand::ProcessBlocks {
                        compacted_blocks: compacted_batch,
                        blocks: blocks_batch,
                    });
                }

                if received >= total {
                    // Drain any remaining out-of-order inbox entries
                    while let Some((block_opt, compacted_opt)) = inbox.remove(&cursor) {
                        let mut c = vec![];
                        let mut b = vec![];
                        if let Some(bytes) = compacted_opt { c.push((cursor, bytes)); }
                        if let Some(block) = block_opt { b.push(block); }
                        forwarded += 1;
                        cursor += 1;
                        if !c.is_empty() || !b.is_empty() {
                            let _ = bp_tx.send(BlockProcessorCommand::ProcessBlocks {
                                compacted_blocks: c,
                                blocks: b,
                            });
                        }
                    }
                    break 'outer;
                }

                if !got_any && !has_data {
                    sleep(Duration::from_millis(5));
                }
            }

            try_info!(ctx_d, "BlkDispatcher: forwarded {forwarded}/{total} blocks");
            // NOTE: do NOT send Terminate here — the caller continues using
            // the BlockProcessor for RPC blocks above our max height.
        })
        .expect("unable to spawn BlkDispatcher thread");

    // Wait for all reader threads
    for handle in reader_handles {
        let _ = handle.join();
    }

    // Wait for dispatcher to finish forwarding
    let _ = dispatcher.join();

    let elapsed = t0.elapsed();
    let bps = if elapsed.as_secs() > 0 { total / elapsed.as_secs() } else { total };
    try_info!(
        ctx,
        "BlkPipeline: streamed {} blocks in {:.1}s ({} blocks/s via direct .blk reads)",
        total,
        elapsed.as_secs_f64(),
        bps
    );

    Ok(())
}
