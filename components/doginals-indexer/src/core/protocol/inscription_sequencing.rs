use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    hash::BuildHasherDefault,
    sync::{mpsc::channel, Arc},
};

use bitcoin::Network;
use config::Config;
use crossbeam_channel::unbounded;
use dashmap::DashMap;
use deadpool_postgres::Transaction;
use dogecoin::{
    try_debug, try_error, try_info,
    types::{
        BlockIdentifier, DogecoinBlockData, DogecoinNetwork, DogecoinTransactionData,
        DoginalInscriptionCurseType, DoginalInscriptionTransferDestination, DoginalOperation,
        TransactionBytesCursor, TransactionIdentifier,
    },
    utils::Context,
};
use doginals::{dogespell::Dogespell, koinu::Koinu};
use fxhash::FxHasher;

use super::{
    koinu_numbering::{compute_koinu_number, TraversalResult},
    koinu_tracking::compute_koinupoint_post_transfer,
    sequence_cursor::SequenceCursor,
};
use crate::{
    core::{protocol::koinu_tracking::UNBOUND_INSCRIPTION_KOINUPOINT, resolve_absolute_pointer},
    db::{self, doginals_pg},
    utils::format_inscription_id,
};

/// Parallelize the computation of doginals numbers for inscriptions present in a block.
#[allow(clippy::type_complexity)]
pub fn parallelize_inscription_data_computations(
    block: &DogecoinBlockData,
    next_blocks: &[DogecoinBlockData],
    cache_l1: &mut BTreeMap<(TransactionIdentifier, usize, u64), TraversalResult>,
    cache_l2: &Arc<DashMap<(u32, [u8; 8]), TransactionBytesCursor, BuildHasherDefault<FxHasher>>>,
    config: &Config,
    ctx: &Context,
) -> Result<bool, String> {
    let inner_ctx = ctx.clone();
    let block_height = block.block_identifier.index;

    try_debug!(
        inner_ctx,
        "Inscriptions data computation for block #{block_height} started"
    );

    let (transactions_ids, l1_cache_hits) = get_transactions_to_process(block, cache_l1);
    let has_transactions_to_process = !transactions_ids.is_empty() || !l1_cache_hits.is_empty();
    if !has_transactions_to_process {
        try_debug!(
            inner_ctx,
            "No reveal transactions found at block #{block_height}"
        );
        return Ok(false);
    }

    let expected_traversals = transactions_ids.len() + l1_cache_hits.len();
    let (traversal_tx, traversal_rx) = unbounded();

    let mut tx_thread_pool = vec![];
    let mut thread_pool_handles = vec![];
    let blocks_db = Arc::new(db::blocks::open_blocks_db_with_retry(false, config, ctx));

    let thread_pool_capacity = config.resources.get_optimal_thread_pool_capacity();
    for thread_index in 0..thread_pool_capacity {
        let (tx, rx) = channel();
        tx_thread_pool.push(tx);

        let moved_traversal_tx = traversal_tx.clone();
        let moved_ctx = inner_ctx.clone();
        let moved_config = config.clone();

        let local_cache = cache_l2.clone();
        let local_db = blocks_db.clone();

        let handle = hiro_system_kit::thread_named("Worker")
            .spawn(move || {
                while let Ok(Some((
                    transaction_id,
                    block_identifier,
                    input_index,
                    inscription_pointer,
                    prioritary,
                ))) = rx.recv()
                {
                    let traversal: Result<(TraversalResult, u64, _), String> = compute_koinu_number(
                        &block_identifier,
                        &transaction_id,
                        input_index,
                        inscription_pointer,
                        &local_cache,
                        &local_db,
                        &moved_config,
                        &moved_ctx,
                    );
                    let _ = moved_traversal_tx.send((traversal, prioritary, thread_index));
                }
            })
            .expect("unable to spawn thread");
        thread_pool_handles.push(handle);
    }

    let mut round_robin_thread_index = 0;
    for key in l1_cache_hits.iter() {
        if let Some(entry) = cache_l1.get(key) {
            let _ = traversal_tx.send((
                Ok((entry.clone(), key.2, vec![])),
                true,
                round_robin_thread_index,
            ));
            round_robin_thread_index = (round_robin_thread_index + 1) % thread_pool_capacity;
        }
    }

    let next_block_heights = next_blocks
        .iter()
        .map(|b| format!("{}", b.block_identifier.index))
        .collect::<Vec<_>>();

    try_debug!(
        inner_ctx,
        "Number of inscriptions in block #{block_height} to process: {num_inscriptions} \
        (L1 cache hits: {l1_hits}, queue: [{next_heights}], L1 cache len: {l1_len}, L2 cache len: {l2_len})",
        num_inscriptions = transactions_ids.len(),
        l1_hits = l1_cache_hits.len(),
        next_heights = next_block_heights.join(", "),
        l1_len = cache_l1.len(),
        l2_len = cache_l2.len(),
    );

    let mut priority_queue = VecDeque::new();
    let mut warmup_queue = VecDeque::new();

    for (transaction_id, input_index, inscription_pointer) in transactions_ids.into_iter() {
        priority_queue.push_back((
            transaction_id,
            block.block_identifier.clone(),
            input_index,
            inscription_pointer,
            true,
        ));
    }

    for thread in &tx_thread_pool {
        let _ = thread.send(priority_queue.pop_front());
    }
    for thread in &tx_thread_pool {
        let _ = thread.send(priority_queue.pop_front());
    }

    let mut next_block_iter = next_blocks.iter();
    let mut traversals_received = 0;
    while let Ok((traversal_result, prioritary, thread_index)) = traversal_rx.recv() {
        if prioritary {
            traversals_received += 1;
        }
        match traversal_result {
            Ok((traversal, inscription_pointer, _)) => {
                try_debug!(
                    inner_ctx,
                    "Completed doginal number retrieval for Satpoint {tx_hash}:{input_index}:{inscription_pointer} \
                    (block: #{coinbase_height}:{coinbase_offset}, transfers: {transfers}, \
                    progress: {traversals_received}/{expected_traversals}, priority queue: {prioritary}, thread: {thread_index})",
                    tx_hash = &traversal.transaction_identifier_inscription.hash,
                    input_index = traversal.inscription_input_index,
                    coinbase_height = traversal.get_doginal_coinbase_height(),
                    coinbase_offset = traversal.get_doginal_coinbase_offset(),
                    transfers = traversal.transfers
                );
                cache_l1.insert(
                    (
                        traversal.transaction_identifier_inscription.clone(),
                        traversal.inscription_input_index,
                        inscription_pointer,
                    ),
                    traversal,
                );
            }
            Err(e) => {
                try_error!(inner_ctx, "Unable to compute inscription's Satoshi: {e}");
            }
        }

        if traversals_received == expected_traversals {
            break;
        }

        if let Some(w) = priority_queue.pop_front() {
            let _ = tx_thread_pool[thread_index].send(Some(w));
        } else if let Some(w) = warmup_queue.pop_front() {
            let _ = tx_thread_pool[thread_index].send(Some(w));
        } else if let Some(next_block) = next_block_iter.next() {
            let (transactions_ids, _) = get_transactions_to_process(next_block, cache_l1);

            try_info!(
                inner_ctx,
                "Number of inscriptions in block #{block_height} to pre-process: {num_inscriptions}",
                num_inscriptions = transactions_ids.len()
            );

            for (transaction_id, input_index, inscription_pointer) in transactions_ids.into_iter() {
                warmup_queue.push_back((
                    transaction_id,
                    next_block.block_identifier.clone(),
                    input_index,
                    inscription_pointer,
                    false,
                ));
            }
            let _ = tx_thread_pool[thread_index].send(warmup_queue.pop_front());
        }
    }
    try_debug!(
        inner_ctx,
        "Inscriptions data computation for block #{block_height} collected"
    );

    for tx in tx_thread_pool.iter() {
        if let Ok((Ok((traversal, inscription_pointer, _)), _prioritary, thread_index)) =
            traversal_rx.try_recv()
        {
            try_debug!(
                inner_ctx,
                "Completed doginal number retrieval for Satpoint {tx_hash}:{input_index}:{inscription_pointer} \
                (block: #{coinbase_height}:{coinbase_offset}, transfers: {transfers}, pre-retrieval, thread: {thread_index})",
                tx_hash = &traversal.transaction_identifier_inscription.hash,
                input_index = traversal.inscription_input_index,
                coinbase_height = traversal.get_doginal_coinbase_height(),
                coinbase_offset = traversal.get_doginal_coinbase_offset(),
                transfers = traversal.transfers
            );
            cache_l1.insert(
                (
                    traversal.transaction_identifier_inscription.clone(),
                    traversal.inscription_input_index,
                    inscription_pointer,
                ),
                traversal,
            );
        }
        let _ = tx.send(None);
    }

    let _ = hiro_system_kit::thread_named("Garbage collection").spawn(move || {
        for handle in thread_pool_handles.into_iter() {
            let _ = handle.join();
        }
    });

    try_debug!(
        inner_ctx,
        "Inscriptions data computation for block #{block_height} ended"
    );

    Ok(has_transactions_to_process)
}

#[allow(clippy::type_complexity)]
fn get_transactions_to_process(
    block: &DogecoinBlockData,
    cache_l1: &mut BTreeMap<(TransactionIdentifier, usize, u64), TraversalResult>,
) -> (
    HashSet<(TransactionIdentifier, usize, u64)>,
    Vec<(TransactionIdentifier, usize, u64)>,
) {
    let mut transactions_ids = HashSet::new();
    let mut l1_cache_hits = vec![];

    for tx in block.transactions.iter().skip(1) {
        let inputs = tx
            .metadata
            .inputs
            .iter()
            .map(|i| i.previous_output.value)
            .collect::<Vec<u64>>();

        for doginal_event in tx.metadata.doginal_operations.iter() {
            let inscription_data = match doginal_event {
                DoginalOperation::InscriptionRevealed(inscription_data) => inscription_data,
                DoginalOperation::InscriptionTransferred(_) => continue,
            };

            let (input_index, relative_offset) = match inscription_data.inscription_pointer {
                Some(pointer) => resolve_absolute_pointer(&inputs, pointer),
                None => (inscription_data.inscription_input_index, 0),
            };

            let key = (
                tx.transaction_identifier.clone(),
                input_index,
                relative_offset,
            );
            if cache_l1.contains_key(&key) {
                l1_cache_hits.push(key);
                continue;
            }

            if transactions_ids.contains(&key) {
                continue;
            }

            transactions_ids.insert(key);
        }
    }
    (transactions_ids, l1_cache_hits)
}

pub fn get_jubilee_block_height(_network: &DogecoinNetwork) -> u64 {
    u64::MAX
}

pub fn get_dogecoin_network(network: &DogecoinNetwork) -> Network {
    match network {
        DogecoinNetwork::Mainnet => Network::Bitcoin,
        DogecoinNetwork::Regtest => Network::Regtest,
        DogecoinNetwork::Testnet => Network::Testnet,
        DogecoinNetwork::Signet => Network::Signet,
    }
}

pub async fn update_block_inscriptions_with_consensus_sequence_data(
    block: &mut DogecoinBlockData,
    sequence_cursor: &mut SequenceCursor,
    inscriptions_data: &mut BTreeMap<(TransactionIdentifier, usize, u64), TraversalResult>,
    db_tx: &Transaction<'_>,
    ctx: &Context,
) -> Result<(), String> {
    let mut reinscriptions_data =
        doginals_pg::get_reinscriptions_for_block(inscriptions_data, db_tx).await?;
    let mut sat_overflows = VecDeque::new();
    let block_identifier = block.block_identifier.clone();
    let block_metadata_network = block.metadata.network.clone();
    for (tx_index, tx) in block.transactions.iter_mut().enumerate() {
        update_tx_inscriptions_with_consensus_sequence_data(
            tx,
            tx_index,
            &block_identifier,
            sequence_cursor,
            &block_metadata_network,
            &block_metadata_network,
            inscriptions_data,
            &mut sat_overflows,
            &mut reinscriptions_data,
            db_tx,
            ctx,
        )
        .await?;
    }

    while let Some((tx_index, op_index)) = sat_overflows.pop_front() {
        let DoginalOperation::InscriptionRevealed(ref mut inscription_data) =
            block.transactions[tx_index].metadata.doginal_operations[op_index]
        else {
            continue;
        };

        let is_cursed = inscription_data.curse_type.is_some();
        let inscription_number = sequence_cursor
            .pick_next(is_cursed, block.block_identifier.index, &block.metadata.network, db_tx)
            .await?;
        inscription_data.inscription_number = inscription_number;
        sequence_cursor.increment(is_cursed, db_tx).await?;

        let unbound_sequence = sequence_cursor.increment_unbound(db_tx).await?;
        inscription_data.koinupoint_post_inscription =
            format!("{UNBOUND_INSCRIPTION_KOINUPOINT}:{unbound_sequence}");
        inscription_data.doginal_offset = unbound_sequence as u64;
        inscription_data.unbound_sequence = Some(unbound_sequence);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn update_tx_inscriptions_with_consensus_sequence_data(
    tx: &mut DogecoinTransactionData,
    tx_index: usize,
    block_identifier: &BlockIdentifier,
    sequence_cursor: &mut SequenceCursor,
    network: &DogecoinNetwork,
    block_metadata_network: &DogecoinNetwork,
    // block: &DogecoinBlockData,
    inscriptions_data: &mut BTreeMap<(TransactionIdentifier, usize, u64), TraversalResult>,
    sats_overflows: &mut VecDeque<(usize, usize)>,
    reinscriptions_data: &mut HashMap<u64, String>,
    db_tx: &Transaction<'_>,
    ctx: &Context,
) -> Result<bool, String> {
    if tx.metadata.doginal_operations.is_empty() {
        return Ok(false);
    }

    let tx_input_values = tx
        .metadata
        .inputs
        .iter()
        .map(|i| i.previous_output.value)
        .collect::<Vec<u64>>();

    let mut mut_operations = vec![];
    mut_operations.append(&mut tx.metadata.doginal_operations);

    let mut inscription_subindex = 0;
    for (op_index, op) in mut_operations.iter_mut().enumerate() {
        let (mut is_cursed, inscription) = match op {
            DoginalOperation::InscriptionRevealed(inscription) => {
                (inscription.curse_type.as_ref().is_some(), inscription)
            }
            DoginalOperation::InscriptionTransferred(_) => continue,
        };

        let (input_index, relative_offset) = match inscription.inscription_pointer {
            Some(pointer) => resolve_absolute_pointer(&tx_input_values, pointer),
            None => (inscription.inscription_input_index, 0),
        };

        let transaction_identifier = tx.transaction_identifier.clone();
        let inscription_id = format_inscription_id(&transaction_identifier, inscription_subindex);
        let traversal =
            match inscriptions_data.get(&(transaction_identifier, input_index, relative_offset)) {
                Some(traversal) => traversal,
                None => {
                    return Err(format!(
                        "Unable to retrieve backward traversal result for inscription in tx {}",
                        tx.transaction_identifier.hash
                    ));
                }
            };

        let mut inscription_number = sequence_cursor
            .pick_next(is_cursed, block_identifier.index, network, db_tx)
            .await?;
        let mut curse_type_override = None;
        if !is_cursed {
            if reinscriptions_data.get(&traversal.doginal_number).is_some() {
                is_cursed = true;
                inscription_number = sequence_cursor
                    .pick_next(is_cursed, block_identifier.index, network, db_tx)
                    .await?;
                curse_type_override = Some(DoginalInscriptionCurseType::Reinscription);
                Dogespell::Reinscription.set(&mut inscription.dogespells);
            }
        };

        inscription.inscription_id = inscription_id;
        inscription.inscription_number = inscription_number;
        inscription.doginal_offset = traversal.get_doginal_coinbase_offset();
        inscription.doginal_block_height = traversal.get_doginal_coinbase_height();
        inscription.doginal_number = traversal.doginal_number;
        inscription.transfers_pre_inscription = traversal.transfers;
        inscription.inscription_fee = tx.metadata.fee;
        inscription.tx_index = tx_index;
        inscription.curse_type = match curse_type_override {
            Some(curse_type) => Some(curse_type),
            None => inscription.curse_type.take(),
        };

        inscription.dogespells |= Koinu(traversal.doginal_number).dogespells();
        if is_cursed {
            if block_identifier.index >= get_jubilee_block_height(block_metadata_network) {
                Dogespell::Vindicated.set(&mut inscription.dogespells);
            } else {
                Dogespell::Cursed.set(&mut inscription.dogespells);
            }
        }

        let (destination, koinupoint_post_transfer, output_value) =
            compute_koinupoint_post_transfer(&*tx, input_index, relative_offset, &get_dogecoin_network(network), ctx);
        inscription.koinupoint_post_inscription = koinupoint_post_transfer;
        inscription_subindex += 1;

        match destination {
            DoginalInscriptionTransferDestination::SpentInFees => {
                sats_overflows.push_back((tx_index, op_index));
                Dogespell::Unbound.set(&mut inscription.dogespells);
                continue;
            }
            DoginalInscriptionTransferDestination::Burnt(_) => {
                Dogespell::Burned.set(&mut inscription.dogespells);
            }
            DoginalInscriptionTransferDestination::Transferred(address) => {
                inscription.inscription_output_value = output_value.unwrap_or(0);
                inscription.inscriber_address = Some(address);
                if output_value.is_none() {
                    Dogespell::Lost.set(&mut inscription.dogespells);
                }
            }
        };

        if !is_cursed {
            reinscriptions_data.insert(traversal.doginal_number, traversal.get_inscription_id());
        }

        try_debug!(
            ctx,
            "Inscription reveal {inscription_id} (#{inscription_number}) detected on Satoshi {doginal_number} \
            at block #{block_index}",
            inscription_id = &inscription.inscription_id,
            inscription_number = inscription.get_inscription_number(),
            doginal_number = inscription.doginal_number,
            block_index = block_identifier.index
        );
        sequence_cursor.increment(is_cursed, db_tx).await?;
    }
    tx.metadata.doginal_operations.append(&mut mut_operations);

    Ok(true)
}

#[cfg(test)]
mod test {
    // No test code or imports
}