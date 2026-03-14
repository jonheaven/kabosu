use dogecoin::types::dogecoin::TxIn;
use tokio_postgres::Transaction;
use bitcoin::{ScriptBuf, Address};
use crate::db::cache::transaction_location::TransactionLocation;
use std::collections::{HashMap, VecDeque};
use dogecoin::{try_debug, try_warn, utils::Context};
use doginals_parser::DuneId;
use lru::LruCache;
use super::input_dune_balance::InputDuneBalance;
use crate::db::models::db_ledger_operation::DbLedgerOperation;
use crate::db::models::db_ledger_entry::DbLedgerEntry;
use crate::db::models::db_dune::DbDune;
use crate::db::pg_get_input_dune_balances;

// ...existing code...
// ...existing code...
// ...existing code...
// Remove unused imports; move to test modules
// ...existing code...
// ...existing code...

/// Takes all transaction inputs and transforms them into dune balances to be allocated for operations. Looks inside an output LRU
/// cache and the DB when there are cache misses.
///
/// # Arguments
///
/// * `inputs` - Raw transaction inputs
/// * `block_output_cache` - Cache with output balances produced by the current block
/// * `output_cache` - LRU cache with output balances
/// * `db_tx` - DB transaction
/// * `ctx` - Context
pub async fn input_dune_balances_from_tx_inputs(
    inputs: &[TxIn],
    block_output_cache: &HashMap<(String, u32), HashMap<DuneId, Vec<InputDuneBalance>>>,
    output_cache: &mut LruCache<(String, u32), HashMap<DuneId, Vec<InputDuneBalance>>>,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> HashMap<DuneId, VecDeque<InputDuneBalance>> {
    // Maps input index to all of its dune balances. Useful in order to keep dune inputs in order.
    let mut indexed_input_dunes = HashMap::new();
    let mut cache_misses = vec![];

    // Look in both current block output cache and in long term LRU cache.
    for (i, input) in inputs.iter().enumerate() {
        let tx_id = input.previous_output.txid.clone();
        let vout = input.previous_output.vout;
        let k = (tx_id.hash.clone(), vout);
        if let Some(map) = block_output_cache.get(&k) {
            indexed_input_dunes.insert(i as u32, map.clone());
        } else if let Some(map) = output_cache.get(&k) {
            indexed_input_dunes.insert(i as u32, map.clone());
        } else {
            cache_misses.push((i as u32, tx_id.hash.clone(), vout));
        }
    }
    // Look for cache misses in database. We don't need to `flush` the DB cache here because we've already looked in the current
    // block's output cache.
    if !cache_misses.is_empty() {
        let output_balances = pg_get_input_dune_balances(cache_misses, db_tx, ctx).await;
        indexed_input_dunes.extend(output_balances);
    }

    let mut final_input_dunes: HashMap<DuneId, VecDeque<InputDuneBalance>> = HashMap::new();
    let mut input_keys: Vec<u32> = indexed_input_dunes.keys().copied().collect();
    input_keys.sort();
    for key in input_keys.iter() {
        let input_value = indexed_input_dunes.get(key).unwrap();
        for (dune_id, vec) in input_value.iter() {
            if let Some(dune) = final_input_dunes.get_mut(dune_id) {
                dune.extend(vec.clone());
            } else {
                final_input_dunes.insert(*dune_id, VecDeque::from(vec.clone()));
            }
        }
    }
    final_input_dunes
}

/// Moves data from the current block's output cache to the long-term LRU output cache. Clears the block output cache when done.
///
/// # Arguments
///
/// * `block_output_cache` - Block output cache
/// * `output_cache` - Output LRU cache
pub fn move_block_output_cache_to_output_cache(
    block_output_cache: &mut HashMap<(String, u32), HashMap<DuneId, Vec<InputDuneBalance>>>,
    output_cache: &mut LruCache<(String, u32), HashMap<DuneId, Vec<InputDuneBalance>>>,
) {
    for (k, block_output_map) in block_output_cache.iter() {
        if let Some(v) = output_cache.get_mut(k) {
            for (dune_id, balances) in block_output_map.iter() {
                if let Some(dune_balance) = v.get_mut(dune_id) {
                    dune_balance.extend(balances.clone());
                } else {
                    v.insert(*dune_id, balances.clone());
                }
            }
        } else {
            output_cache.push(k.clone(), block_output_map.clone());
        }
    }
    block_output_cache.clear();
}

/// Creates a new ledger entry while incrementing the `next_event_index`.
#[allow(clippy::too_many_arguments)]
pub fn new_sequential_ledger_entry(
    location: &TransactionLocation,
    amount: Option<u128>,
    dune_id: DuneId,
    output: Option<u32>,
    address: Option<&String>,
    receiver_address: Option<&String>,
    operation: DbLedgerOperation,
    next_event_index: &mut u32,
) -> DbLedgerEntry {
    let entry = DbLedgerEntry::from_values(
        amount,
        dune_id,
        &location.block_hash,
        location.block_height,
        location.tx_index,
        *next_event_index,
        &location.tx_id,
        output,
        address,
        receiver_address,
        operation,
        location.timestamp,
    );
    *next_event_index += 1;
    entry
}

/// Moves dune balance from transaction inputs into a transaction output.
///
/// # Arguments
///
/// * `location` - Transaction location.
/// * `output` - Output where dunes will be moved to. If `None`, dunes are burned.
/// * `dune_id` - Dune that is being moved.
/// * `input_balances` - Balances input to this transaction for this dune. This value will be modified by the moves happening in
///   this function.
/// * `outputs` - Transaction outputs eligible to receive dunes.
/// * `amount` - Amount of balance to move. If value is zero, all inputs will be moved to the output.
/// * `next_event_index` - Next sequential event index to create. This value will be modified.
/// * `ctx` - Context.
#[allow(clippy::too_many_arguments)]
pub fn move_dune_balance_to_output(
    location: &TransactionLocation,
    output: Option<u32>,
    dune_id: &DuneId,
    input_balances: &mut VecDeque<InputDuneBalance>,
    outputs: &HashMap<u32, ScriptBuf>,
    amount: u128,
    next_event_index: &mut u32,
    ctx: &Context,
) -> Vec<DbLedgerEntry> {
    let mut results = vec![];
    // Who is this balance going to?
    let receiver_address = if let Some(output) = output {
        match outputs.get(&output) {
            Some(script) => match Address::from_script(script, location.network) {
                Ok(address) => Some(address.to_string()),
                Err(e) => {
                    try_warn!(
                        ctx,
                        "Unable to decode address for output {output}, {e} {location}"
                    );
                    None
                }
            },
            None => {
                try_debug!(
                    ctx,
                    "Attempted move to non-eligible output {output}, dunes will be burnt {location}"
                );
                None
            }
        }
    } else {
        None
    };
    let operation = if receiver_address.is_some() {
        DbLedgerOperation::Send
    } else {
        DbLedgerOperation::Burn
    };

    // Gather balance to be received by taking it from the available inputs until the amount to move is satisfied.
    let mut total_sent = 0;
    let mut senders = vec![];
    loop {
        let Some(input_bal) = input_balances.pop_front() else { break; };
        let balance_taken = if amount == 0 {
            input_bal.balance
        } else {
            input_bal.balance.min(amount - total_sent)
        };
        total_sent += balance_taken;
        if let Some(sender_address) = input_bal.address.clone() {
            senders.push((balance_taken, sender_address));
        }
        if balance_taken < input_bal.balance {
            input_balances.push_front(InputDuneBalance {
                dune_id: input_bal.dune_id.clone(),
                balance: input_bal.balance - balance_taken,
                txid: input_bal.txid.clone(),
                vout: input_bal.vout,
                address: input_bal.address.clone(),
                block_height: input_bal.block_height,
                timestamp: input_bal.timestamp,
            });
            break;
        }
        if total_sent == amount {
            break;
        }
    }
    // Add the "receive" entry, if applicable.
    if receiver_address.is_some() && total_sent > 0 {
        results.push(new_sequential_ledger_entry(
            location,
            Some(total_sent),
            *dune_id,
            output,
            receiver_address.as_ref(),
            None,
            DbLedgerOperation::Receive,
            next_event_index,
        ));
        try_debug!(
            ctx,
            "{operation} {dune_id} ({total_sent}) {address} {location}",
            operation = DbLedgerOperation::Receive.to_string(),
            address = receiver_address.as_ref().unwrap(),
        );
    }
    // Add the "send"/"burn" entries.
    for (balance_taken, sender_address) in senders.iter() {
        results.push(new_sequential_ledger_entry(
            location,
            Some(*balance_taken),
            *dune_id,
            output,
            Some(sender_address),
            receiver_address.as_ref(),
            operation.clone(),
            next_event_index,
        ));
        try_debug!(
            ctx,
            "{operation} {dune_id} ({balance_taken}) {sender_address} -> {receiver_address:?} {location}"
        );
    }
    results
}

/// Determines if a mint is valid depending on the dune's mint terms.
pub fn is_dune_mintable(
    db_dune: &DbDune,
    total_mints: u128,
    location: &TransactionLocation,
) -> bool {
    if db_dune.cenotaph {
        return false;
    }
    if db_dune.terms_amount.is_none() {
        return false;
    }
    if let Some(terms_cap) = db_dune.terms_cap {
        if total_mints >= terms_cap.0 {
            return false;
        }
    }
    if let Some(terms_height_start) = db_dune.terms_height_start {
        if location.block_height < terms_height_start.0 {
            return false;
        }
    }
    if let Some(terms_height_end) = db_dune.terms_height_end {
        if location.block_height > terms_height_end.0 {
            return false;
        }
    }
    if let Some(terms_offset_start) = db_dune.terms_offset_start {
        if location.block_height < db_dune.block_height.0 + terms_offset_start.0 {
            return false;
        }
    }
    if let Some(terms_offset_end) = db_dune.terms_offset_end {
        if location.block_height > db_dune.block_height.0 + terms_offset_end.0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod test {
    mod move_balance {
        use std::collections::{HashMap, VecDeque};

        use bitcoin::ScriptBuf;
        use dogecoin::utils::Context;
        use doginals_parser::DuneId;
        use maplit::hashmap;

        use crate::db::cache::input_dune_balance::InputDuneBalance;
        use crate::db::cache::transaction_location::TransactionLocation;
        use crate::db::cache::utils::move_dune_balance_to_output;
        use crate::db::models::db_ledger_operation::DbLedgerOperation;
        use bitcoin::Txid;

        fn dummy_eligible_output() -> HashMap<u32, ScriptBuf> {
            hashmap! {
                0u32 => ScriptBuf::from_hex(
                    "5120388dfba1b0069bbb0ad5eef62c1a94c46e91a3454accf40bf34b80f75e2708db",
                )
                .unwrap()
            }
        }

        #[test]
        fn ledger_writes_receive_before_send() {
            let address =
                Some("bc1p8zxlhgdsq6dmkzk4ammzcx55c3hfrg69ftx0gzlnfwq0wh38prds0nzqwf".to_string());
            let mut available_inputs = VecDeque::new();
            let mut input1 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 1000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: address.clone(),
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input1);
            let mut input2 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 1000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: None,
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input2);
            let eligible_outputs = dummy_eligible_output();
            let mut next_event_index = 0;

            let results = move_dune_balance_to_output(
                &TransactionLocation::dummy(),
                Some(0),
                &DuneId::new(840000, 25).unwrap(),
                &mut available_inputs,
                &eligible_outputs,
                0,
                &mut next_event_index,
                &Context::empty(),
            );

            let receive = results.get(0).unwrap();
            assert_eq!(receive.event_index.0, 0u32);
            assert_eq!(receive.operation, DbLedgerOperation::Receive);
            assert_eq!(receive.amount.unwrap().0, 2000u128);
            // ...existing code...
            let send = results.get(1).unwrap();
            assert_eq!(send.event_index.0, 1u32);
            assert_eq!(send.operation, DbLedgerOperation::Send);
            assert_eq!(send.amount.unwrap().0, 1000u128);
            assert_eq!(results.len(), 2);
            assert_eq!(available_inputs.len(), 0);
        }

        #[test]
        fn move_to_empty_output_is_burned() {
            let address =
                Some("bc1p8zxlhgdsq6dmkzk4ammzcx55c3hfrg69ftx0gzlnfwq0wh38prds0nzqwf".to_string());
            let mut available_inputs = VecDeque::new();
            let mut input1 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 1000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: address.clone(),
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input1);

            let results = move_dune_balance_to_output(
                &TransactionLocation::dummy(),
                None, // Burn
                &DuneId::new(840000, 25).unwrap(),
                &mut available_inputs,
                &HashMap::new(),
                0,
                &mut 0,
                &Context::empty(),
            );

            assert_eq!(results.len(), 1);
            let entry1 = results.get(0).unwrap();
            assert_eq!(entry1.operation, DbLedgerOperation::Burn);
            assert_eq!(entry1.address, address);
            assert_eq!(entry1.amount.unwrap().0, 1000);
            assert_eq!(available_inputs.len(), 0);
        }

        #[test]
        fn moves_partial_input_balance() {
            let mut available_inputs = VecDeque::new();
            let mut input1 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 5000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: None,
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input1);
            let eligible_outputs = dummy_eligible_output();

            let results = move_dune_balance_to_output(
                &TransactionLocation::dummy(),
                Some(0),
                &DuneId::new(840000, 25).unwrap(),
                &mut available_inputs,
                &eligible_outputs,
                1000, // Less than total available in first input.
                &mut 0,
                &Context::empty(),
            );

            assert_eq!(results.len(), 2);
            let entry1 = results.get(0).unwrap();
            assert_eq!(entry1.operation, DbLedgerOperation::Receive);
            assert_eq!(entry1.amount.unwrap().0, 1000);
            let entry2 = results.get(1).unwrap();
            assert_eq!(entry2.operation, DbLedgerOperation::Send);
            assert_eq!(entry2.amount.unwrap().0, 1000);
            // Remainder is still in available inputs.
            let remaining = available_inputs.get(0).unwrap();
            assert_eq!(remaining.balance, 4000);
        }

        #[test]
        fn moves_insufficient_input_balance() {
            let mut available_inputs = VecDeque::new();
            let mut input1 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 1000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: None,
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input1);
            let eligible_outputs = dummy_eligible_output();

            let results = move_dune_balance_to_output(
                &TransactionLocation::dummy(),
                Some(0),
                &DuneId::new(840000, 25).unwrap(),
                &mut available_inputs,
                &eligible_outputs,
                3000, // More than total available in input.
                &mut 0,
                &Context::empty(),
            );

            assert_eq!(results.len(), 2);
            let entry1 = results.get(0).unwrap();
            assert_eq!(entry1.operation, DbLedgerOperation::Receive);
            assert_eq!(entry1.amount.unwrap().0, 1000);
            let entry2 = results.get(1).unwrap();
            assert_eq!(entry2.operation, DbLedgerOperation::Send);
            assert_eq!(entry2.amount.unwrap().0, 1000);
            assert_eq!(available_inputs.len(), 0);
        }

        #[test]
        fn moves_all_remaining_balance() {
            let mut available_inputs = VecDeque::new();
            let mut input1 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 6000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: None,
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input1);
            let mut input2 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 2000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: None,
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input2);
            let mut input3 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 2000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: None,
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input3);
            let eligible_outputs = dummy_eligible_output();

            let results = move_dune_balance_to_output(
                &TransactionLocation::dummy(),
                Some(0),
                &DuneId::new(840000, 25).unwrap(),
                &mut available_inputs,
                &eligible_outputs,
                0, // Move all.
                &mut 0,
                &Context::empty(),
            );

            assert_eq!(results.len(), 4);
            let entry1 = results.get(0).unwrap();
            assert_eq!(entry1.operation, DbLedgerOperation::Receive);
            assert_eq!(entry1.amount.unwrap().0, 10000);
            let entry2 = results.get(1).unwrap();
            assert_eq!(entry2.operation, DbLedgerOperation::Send);
            assert_eq!(entry2.amount.unwrap().0, 6000);
            let entry3 = results.get(2).unwrap();
            assert_eq!(entry3.operation, DbLedgerOperation::Send);
            assert_eq!(entry3.amount.unwrap().0, 2000);
            let entry4 = results.get(3).unwrap();
            assert_eq!(entry4.operation, DbLedgerOperation::Send);
            assert_eq!(entry4.amount.unwrap().0, 2000);
            assert_eq!(available_inputs.len(), 0);
        }

        #[test]
        fn move_to_output_with_address_failure_is_burned() {
            let mut available_inputs = VecDeque::new();
            let mut input1 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 1000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: None,
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input1);
            let mut eligible_outputs = HashMap::new();
            // Broken script buf that yields no address.
            eligible_outputs.insert(0u32, ScriptBuf::from_hex("0101010101").unwrap());

            let results = move_dune_balance_to_output(
                &TransactionLocation::dummy(),
                Some(0),
                &DuneId::new(840000, 25).unwrap(),
                &mut available_inputs,
                &eligible_outputs,
                1000,
                &mut 0,
                &Context::empty(),
            );

            assert_eq!(results.len(), 1);
            let entry1 = results.get(0).unwrap();
            assert_eq!(entry1.operation, DbLedgerOperation::Burn);
            assert_eq!(entry1.amount.unwrap().0, 1000);
            assert_eq!(available_inputs.len(), 0);
        }

        #[test]
        fn move_to_nonexistent_output_is_burned() {
            let mut available_inputs = VecDeque::new();
            let mut input1 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 1000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: None,
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input1);
            let eligible_outputs = dummy_eligible_output();

            let results = move_dune_balance_to_output(
                &TransactionLocation::dummy(),
                Some(5), // Output does not exist.
                &DuneId::new(840000, 25).unwrap(),
                &mut available_inputs,
                &eligible_outputs,
                1000,
                &mut 0,
                &Context::empty(),
            );

            assert_eq!(results.len(), 1);
            let entry1 = results.get(0).unwrap();
            assert_eq!(entry1.operation, DbLedgerOperation::Burn);
            assert_eq!(entry1.amount.unwrap().0, 1000);
            assert_eq!(available_inputs.len(), 0);
        }

        #[test]
        fn send_not_generated_on_minted_balance() {
            let mut available_inputs = VecDeque::new();
            let mut input1 = InputDuneBalance {
                dune_id: DuneId::new(840000, 25).unwrap(),
                balance: 1000,
                txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                vout: 0,
                address: None,
                block_height: 0,
                timestamp: 0,
            };
            available_inputs.push_back(input1);
            let eligible_outputs = dummy_eligible_output();

            let results = move_dune_balance_to_output(
                &TransactionLocation::dummy(),
                Some(0),
                &DuneId::new(840000, 25).unwrap(),
                &mut available_inputs,
                &eligible_outputs,
                1000,
                &mut 0,
                &Context::empty(),
            );

            assert_eq!(results.len(), 1);
            let entry1 = results.get(0).unwrap();
            assert_eq!(entry1.operation, DbLedgerOperation::Receive);
            assert_eq!(entry1.amount.unwrap().0, 1000);
            assert_eq!(available_inputs.len(), 0);
        }
    }

    mod mint_validation {
        use postgres::types::{PgNumericU128, PgNumericU64};
        use test_case::test_case;

        use crate::db::{
            cache::{transaction_location::TransactionLocation, utils::is_dune_mintable},
            models::db_dune::DbDune,
        };

        #[test_case(840000 => false; "early block")]
        #[test_case(840500 => false; "late block")]
        #[test_case(840150 => true; "block in window")]
        #[test_case(840100 => true; "first block")]
        #[test_case(840200 => true; "last block")]
        fn mint_block_height_terms_are_validated(block_height: u64) -> bool {
            let mut dune = DbDune::factory();
            dune.terms_height_start(Some(PgNumericU64(840100)));
            dune.terms_height_end(Some(PgNumericU64(840200)));
            let mut location = TransactionLocation::dummy();
            location.block_height(block_height);
            is_dune_mintable(&dune, 0, &location)
        }

        #[test_case(840000 => false; "early block")]
        #[test_case(840500 => false; "late block")]
        #[test_case(840150 => true; "block in window")]
        #[test_case(840100 => true; "first block")]
        #[test_case(840200 => true; "last block")]
        fn mint_block_offset_terms_are_validated(block_height: u64) -> bool {
            let mut dune = DbDune::factory();
            dune.terms_offset_start(Some(PgNumericU64(100)));
            dune.terms_offset_end(Some(PgNumericU64(200)));
            let mut location = TransactionLocation::dummy();
            location.block_height(block_height);
            is_dune_mintable(&dune, 0, &location)
        }

        #[test_case(0 => true; "first mint")]
        #[test_case(49 => true; "last mint")]
        #[test_case(50 => false; "out of range")]
        fn mint_cap_is_validated(cap: u128) -> bool {
            let mut dune = DbDune::factory();
            dune.terms_cap(Some(PgNumericU128(50)));
            is_dune_mintable(&dune, cap, &TransactionLocation::dummy())
        }
    }

    mod sequential_ledger_entry {
        use doginals_parser::DuneId;

        use crate::db::{
            cache::{
                transaction_location::TransactionLocation, utils::new_sequential_ledger_entry,
            },
            models::db_ledger_operation::DbLedgerOperation,
        };

        #[test]
        fn increments_event_index() {
            let location = TransactionLocation::dummy();
            let dune_id = DuneId::new(840000, 25).unwrap();
            let address =
                Some("bc1p8zxlhgdsq6dmkzk4ammzcx55c3hfrg69ftx0gzlnfwq0wh38prds0nzqwf".to_string());
            let mut event_index = 0u32;

            let event0 = new_sequential_ledger_entry(
                &location,
                Some(100),
                dune_id,
                Some(0),
                address.as_ref(),
                None,
                DbLedgerOperation::Receive,
                &mut event_index,
            );
            assert_eq!(event0.event_index.0, 0);
            assert_eq!(event0.amount.unwrap().0, 100);
            assert_eq!(event0.address, address);

            let event1 = new_sequential_ledger_entry(
                &location,
                Some(300),
                dune_id,
                Some(0),
                None,
                None,
                DbLedgerOperation::Receive,
                &mut event_index,
            );
            assert_eq!(event1.event_index.0, 1);
            assert_eq!(event1.amount.unwrap().0, 300);
            assert_eq!(event1.address, None);

            assert_eq!(event_index, 2);
        }
    }

    mod input_balances {
        use std::num::NonZeroUsize;

        use bitcoin::{OutPoint, TxIn, ScriptBuf, Sequence, Witness};
        use dogecoin::utils::Context;
        use doginals_parser::DuneId;
        use lru::LruCache;
        use maplit::hashmap;

        use crate::db::cache::input_dune_balance::InputDuneBalance;
        // Removed unused import for TransactionLocation
        use crate::db::cache::utils::input_dune_balances_from_tx_inputs;
        use crate::db::models::db_ledger_entry::DbLedgerEntry;
        use crate::db::models::db_ledger_operation::DbLedgerOperation;
        use crate::db::pg_insert_ledger_entries;
        use crate::db::pg_test_client;
        use crate::db::pg_test_roll_back_migrations;
        use bitcoin::Txid;

        #[tokio::test]
        async fn from_block_cache() {
            let inputs = vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_hex("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                    vout: 1,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence(0),
                witness: Witness::new(),
            }];
            let dune_id = DuneId::new(840000, 25).unwrap();
            let block_output_cache = hashmap! {
                ("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b"
                            .to_string(), 1) => hashmap! {
                                dune_id => vec![InputDuneBalance {
                                    dune_id: dune_id,
                                    balance: 2000,
                                    txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                                    vout: 1,
                                    address: None,
                                    block_height: 0,
                                    timestamp: 0,
                                }]
                            }
            };
            let mut output_cache = LruCache::new(NonZeroUsize::new(1).unwrap());
            let ctx = Context::empty();

            let mut pg_client = pg_test_client(true, &ctx).await;
            let mut db_tx = pg_client.transaction().await.unwrap();
            let results = input_dune_balances_from_tx_inputs(
                inputs.as_slice(),
                &block_output_cache,
                &mut output_cache,
                &mut db_tx,
                &ctx,
            )
            .await;
            let _ = db_tx.rollback().await;
            pg_test_roll_back_migrations(&mut pg_client, &ctx).await;

            assert_eq!(results.len(), 1);
            let dune_results = results.get(&dune_id).unwrap();
            assert_eq!(dune_results.len(), 1);
            let input_bal = dune_results.get(0).unwrap();
            assert_eq!(input_bal.address, Option::<String>::None);
            assert_eq!(input_bal.balance, 2000);
        }

        #[tokio::test]
        async fn from_lru_cache() {
            let inputs = vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_hex("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                    vout: 1,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence(0),
                witness: Witness::new(),
            }];
            let dune_id = DuneId::new(840000, 25).unwrap();
            let block_output_cache = hashmap! {};
            let mut output_cache = LruCache::new(NonZeroUsize::new(1).unwrap());
            output_cache.put(
                (
                    "045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b".to_string(),
                    1,
                ),
                hashmap! {
                    dune_id => vec![InputDuneBalance { address: None, balance: 2000, dune_id, txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(), vout: 1, block_height: 0, timestamp: 0 }]
                },
            );
            let ctx = Context::empty();

            let mut pg_client = pg_test_client(true, &ctx).await;
            let mut db_tx = pg_client.transaction().await.unwrap();
            let results = input_dune_balances_from_tx_inputs(
                inputs.as_slice(),
                &block_output_cache,
                &mut output_cache,
                &mut db_tx,
                &ctx,
            )
            .await;
            let _ = db_tx.rollback().await;
            pg_test_roll_back_migrations(&mut pg_client, &ctx).await;

            assert_eq!(results.len(), 1);
            let dune_results = results.get(&dune_id).unwrap();
            assert_eq!(dune_results.len(), 1);
            let input_bal = dune_results.get(0).unwrap();
            assert_eq!(input_bal.address, Option::<String>::None);
            assert_eq!(input_bal.balance, 2000);
        }

        #[tokio::test]
        async fn from_db() {
            let inputs = vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_hex("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                    vout: 1,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence(0),
                witness: Witness::new(),
            }];
            let dune_id = DuneId::new(840000, 25).unwrap();
            let block_output_cache = hashmap! {};
            let mut output_cache = LruCache::new(NonZeroUsize::new(1).unwrap());
            let ctx = Context::empty();

            let mut pg_client = pg_test_client(true, &ctx).await;
            let mut db_tx = pg_client.transaction().await.unwrap();

            let entry = DbLedgerEntry::from_values(
                Some(2000),
                dune_id,
                &"0x0000000000000000000044642cc1f64c22579d46a2a149ef2a51f9c98cb622e1".to_string(),
                840000,
                0,
                0,
                &"0x045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b".to_string(),
                Some(1),
                None,
                None,
                DbLedgerOperation::Receive,
                0,
            );
            let _ = pg_insert_ledger_entries(&vec![entry], &mut db_tx, &ctx).await;

            let results = input_dune_balances_from_tx_inputs(
                inputs.as_slice(),
                &block_output_cache,
                &mut output_cache,
                &mut db_tx,
                &ctx,
            )
            .await;
            let _ = db_tx.rollback().await;
            pg_test_roll_back_migrations(&mut pg_client, &ctx).await;

            assert_eq!(results.len(), 1);
            let dune_results = results.get(&dune_id).unwrap();
            assert_eq!(dune_results.len(), 1);
            let input_bal = dune_results.get(0).unwrap();
            assert_eq!(input_bal.address, Option::<String>::None);
            assert_eq!(input_bal.balance, 2000);
        }

        #[tokio::test]
        async fn inputs_without_balances() {
            let inputs = vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_hex("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(),
                    vout: 1,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence(0),
                witness: Witness::new(),
            }];
            let block_output_cache = hashmap! {};
            let mut output_cache = LruCache::new(NonZeroUsize::new(1).unwrap());
            let ctx = Context::empty();

            let mut pg_client = pg_test_client(true, &ctx).await;
            let mut db_tx = pg_client.transaction().await.unwrap();
            let results = input_dune_balances_from_tx_inputs(
                inputs.as_slice(),
                &block_output_cache,
                &mut output_cache,
                &mut db_tx,
                &ctx,
            )
            .await;
            let _ = db_tx.rollback().await;
            pg_test_roll_back_migrations(&mut pg_client, &ctx).await;

            assert_eq!(results.len(), 0);
        }
    }

    mod cache_move {
        use std::num::NonZeroUsize;

        use doginals_parser::DuneId;
        use lru::LruCache;
        use maplit::hashmap;

        use crate::db::cache::input_dune_balance::InputDuneBalance;
        use crate::db::cache::utils::move_block_output_cache_to_output_cache;

        #[test]
        fn moves_to_lru_output_cache_and_clears() {
            let dune_id = DuneId::new(840000, 25).unwrap();
            let mut block_output_cache = hashmap! {
                ("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b"
                            .to_string(), 1) => hashmap! {
                                dune_id => vec![InputDuneBalance { address: None, balance: 2000, dune_id, txid: Txid::from_str("045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b").unwrap(), vout: 1, block_height: 0, timestamp: 0 }]
                            }
            };
            let mut output_cache = LruCache::new(NonZeroUsize::new(1).unwrap());

            move_block_output_cache_to_output_cache(&mut block_output_cache, &mut output_cache);

            let moved_val = output_cache
                .get(&(
                    "045fe33f1174d6a72084e751735a89746a259c6d3e418b65c03ec0740f924c7b".to_string(),
                    1,
                ))
                .unwrap();
            assert_eq!(moved_val.len(), 1);
            let balances = moved_val.get(&dune_id).unwrap();
            assert_eq!(balances.len(), 1);
            let balance = balances.get(0).unwrap();
            assert_eq!(balance.address, Option::<String>::None);
            assert_eq!(balance.balance, 2000);
            assert_eq!(block_output_cache.len(), 0);
        }
    }
}
