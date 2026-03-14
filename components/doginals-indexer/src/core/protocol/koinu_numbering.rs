use std::{hash::BuildHasherDefault, sync::Arc};

use config::Config;
use dashmap::DashMap;
use dogecoin::{
    bitcoincore_rpc::RpcApi,
    try_error, try_warn,
    types::{
        BlockBytesCursor, BlockIdentifier, DoginalInscriptionNumber, TransactionBytesCursor,
        TransactionIdentifier, TransactionInputBytesCursor,
    },
    utils::{dogecoind::dogecoin_get_client, Context},
};
use doginals::{height::Height, koinu::Koinu};
use fxhash::FxHasher;
use serde_json::json;

use crate::db::blocks::find_pinned_block_bytes_at_block_height;

#[derive(Clone, Debug)]
pub struct TraversalResult {
    pub inscription_number: DoginalInscriptionNumber,
    pub inscription_input_index: usize,
    pub transaction_identifier_inscription: TransactionIdentifier,
    pub doginal_number: u64,
    pub transfers: u32,
}

impl TraversalResult {
    pub fn get_doginal_coinbase_height(&self) -> u64 {
        let koinu = Koinu(self.doginal_number);
        koinu.height().n() as u64
    }

    pub fn get_doginal_coinbase_offset(&self) -> u64 {
        let koinu = Koinu(self.doginal_number);
        self.doginal_number - koinu.height().starting_sat().n()
    }

    pub fn get_inscription_id(&self) -> String {
        format!(
            "{}i{}",
            self.transaction_identifier_inscription.get_hash_bytes_str(),
            self.inscription_input_index
        )
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn compute_koinu_number(
    block_identifier: &BlockIdentifier,
    transaction_identifier: &TransactionIdentifier,
    inscription_input_index: usize,
    inscription_pointer: u64,
    traversals_cache: &Arc<
        DashMap<(u32, [u8; 8]), TransactionBytesCursor, BuildHasherDefault<FxHasher>>,
    >,
    blocks_db: &rocksdb::DB,
    config: &Config,
    ctx: &Context,
) -> Result<(TraversalResult, u64, Vec<(u32, [u8; 8], usize)>), String> {
    let mut doginal_offset = inscription_pointer;
    let doginal_block_number = block_identifier.index as u32;
    let txid = transaction_identifier.get_8_hash_bytes();
    let mut back_track = vec![];

    let (mut tx_cursor, mut doginal_block_number) = match traversals_cache
        .get(&(block_identifier.index as u32, txid))
    {
        Some(entry) => {
            let tx = entry.value();
            (
                (
                    tx.inputs[inscription_input_index].txin,
                    tx.inputs[inscription_input_index].vout.into(),
                ),
                tx.inputs[inscription_input_index].block_height,
            )
        }
        None => {
            match find_pinned_block_bytes_at_block_height(doginal_block_number, 3, blocks_db, ctx) {
                None => {
                    return Err(format!("block #{doginal_block_number} not in database"));
                }
                Some(block_bytes) => {
                    let cursor = BlockBytesCursor::new(block_bytes.as_ref());
                    match cursor.find_and_serialize_transaction_with_txid(&txid) {
                        Some(tx) => (
                            (
                                tx.inputs[inscription_input_index].txin,
                                tx.inputs[inscription_input_index].vout.into(),
                            ),
                            tx.inputs[inscription_input_index].block_height,
                        ),
                        None => return Err(format!("txid not in block #{doginal_block_number}")),
                    }
                }
            }
        }
    };

    let mut hops: u32 = 0;

    loop {
        hops += 1;
        if hops as u64 > block_identifier.index {
            return Err(format!(
                "Unable to process transaction {} detected after {hops} iterations. Manual investigation required",
                transaction_identifier.hash
            ));
        }

        if let Some(cached_tx) = traversals_cache.get(&(doginal_block_number, tx_cursor.0)) {
            let tx = cached_tx.value();

            let mut next_found_in_cache = false;
            let mut sats_out = 0;
            for (index, output_value) in tx.outputs.iter().enumerate() {
                if index == tx_cursor.1 {
                    break;
                }
                sats_out += output_value;
            }
            sats_out += doginal_offset;

            let mut sats_in = 0;
            for input in tx.inputs.iter() {
                sats_in += input.txin_value;

                if sats_out < sats_in {
                    doginal_offset = sats_out - (sats_in - input.txin_value);
                    doginal_block_number = input.block_height;
                    tx_cursor = (input.txin, input.vout as usize);
                    next_found_in_cache = true;
                    break;
                }
            }

            if next_found_in_cache {
                continue;
            }

            if sats_in == 0 {
                try_error!(
                    ctx,
                    "Transaction {} is originating from a non spending transaction",
                    transaction_identifier.hash
                );
                return Ok((
                    TraversalResult {
                        inscription_number: DoginalInscriptionNumber::zero(),
                        doginal_number: 0,
                        transfers: 0,
                        inscription_input_index,
                        transaction_identifier_inscription: transaction_identifier.clone(),
                    },
                    inscription_pointer,
                    back_track,
                ));
            }
        }

        let pinned_block_bytes = {
            match find_pinned_block_bytes_at_block_height(doginal_block_number, 3, blocks_db, ctx) {
                Some(block) => block,
                None => {
                    // Block is before our RocksDB start height — fetch the tx directly from RPC.
                    try_warn!(
                        ctx,
                        "block #{doginal_block_number} not in local DB while traversing {}; fetching tx from RPC",
                        transaction_identifier.hash
                    );
                    let rpc_cursor =
                        fetch_tx_cursor_from_rpc(doginal_block_number, txid, config, ctx)?;
                    // Warm the cache so the next iteration takes the fast cached path.
                    traversals_cache.insert((doginal_block_number, tx_cursor.0), rpc_cursor);
                    continue;
                }
            }
        };
        let block_cursor = BlockBytesCursor::new(pinned_block_bytes.as_ref());
        let txid = tx_cursor.0;
        let mut block_cursor_tx_iter = block_cursor.iter_tx();
        let coinbase = block_cursor_tx_iter.next().expect("empty block");

        // evaluate exit condition: did we reach the **final** coinbase transaction
        if coinbase.txid.eq(&txid) {
            let mut intra_coinbase_output_offset = 0;
            for (index, output_value) in coinbase.outputs.iter().enumerate() {
                if index == tx_cursor.1 {
                    break;
                }
                intra_coinbase_output_offset += output_value;
            }
            doginal_offset += intra_coinbase_output_offset;

            let subsidy = Height(doginal_block_number).subsidy();
            if doginal_offset < subsidy {
                // Great!
                break;
            }

            // loop over the transaction fees to detect the right range
            let mut accumulated_fees = subsidy;

            for tx in block_cursor_tx_iter {
                let mut total_in = 0;
                for input in tx.inputs.iter() {
                    total_in += input.txin_value;
                }

                let mut total_out = 0;
                for output_value in tx.outputs.iter() {
                    total_out += output_value;
                }

                let fee = total_in - total_out;
                if accumulated_fees + fee > doginal_offset {
                    // We are looking at the right transaction
                    // Retraverse the inputs to select the index to be picked
                    let offset_within_fee = doginal_offset - accumulated_fees;
                    total_out += offset_within_fee;
                    let mut sats_in = 0;

                    for input in tx.inputs.into_iter() {
                        sats_in += input.txin_value;

                        if sats_in > total_out {
                            doginal_offset = total_out - (sats_in - input.txin_value);
                            doginal_block_number = input.block_height;
                            tx_cursor = (input.txin, input.vout as usize);
                            break;
                        }
                    }
                    break;
                } else {
                    accumulated_fees += fee;
                }
            }
        } else {
            // isolate the target transaction
            let tx_bytes_cursor = match block_cursor.find_and_serialize_transaction_with_txid(&txid)
            {
                Some(entry) => entry,
                None => {
                    return Err(format!(
                        "unable to retrieve tx ancestor {} in block #{doginal_block_number} while traversing satpoint {}:{inscription_input_index}",
                        hex::encode(txid),
                        transaction_identifier.get_hash_bytes_str(),
                    ));
                }
            };

            let mut sats_out = 0;
            for (index, output_value) in tx_bytes_cursor.outputs.iter().enumerate() {
                if index == tx_cursor.1 {
                    break;
                }
                sats_out += output_value;
            }
            sats_out += doginal_offset;

            let mut sats_in = 0;
            for input in tx_bytes_cursor.inputs.iter() {
                sats_in += input.txin_value;

                if sats_out < sats_in {
                    back_track.push((doginal_block_number, tx_cursor.0, tx_cursor.1));
                    traversals_cache
                        .insert((doginal_block_number, tx_cursor.0), tx_bytes_cursor.clone());
                    doginal_offset = sats_out - (sats_in - input.txin_value);
                    doginal_block_number = input.block_height;
                    tx_cursor = (input.txin, input.vout as usize);
                    break;
                }
            }

            if sats_in == 0 {
                try_error!(
                    ctx,
                    "Transaction {} is originating from a non spending transaction",
                    transaction_identifier.hash
                );
                return Ok((
                    TraversalResult {
                        inscription_number: DoginalInscriptionNumber::zero(),
                        doginal_number: 0,
                        transfers: 0,
                        inscription_input_index,
                        transaction_identifier_inscription: transaction_identifier.clone(),
                    },
                    inscription_pointer,
                    back_track,
                ));
            }
        }
    }

    let height = Height(doginal_block_number);
    let doginal_number = height.starting_sat().0 + doginal_offset;

    Ok((
        TraversalResult {
            inscription_number: DoginalInscriptionNumber::zero(),
            doginal_number,
            transfers: hops,
            inscription_input_index,
            transaction_identifier_inscription: transaction_identifier.clone(),
        },
        inscription_pointer,
        back_track,
    ))
}

/// Fetch a `TransactionBytesCursor` for a tx that is not in the local RocksDB blocks store
/// by querying the Dogecoin Core node directly.
/// Used as a fallback when the indexer started mid-chain and ancestor blocks are missing.
fn fetch_tx_cursor_from_rpc(
    block_height: u32,
    target_prefix: [u8; 8],
    config: &Config,
    ctx: &Context,
) -> Result<TransactionBytesCursor, String> {
    let rpc = dogecoin_get_client(&config.dogecoin, ctx);

    // 1. Get the block hash for this height
    let block_hash = rpc
        .get_block_hash(block_height as u64)
        .map_err(|e| format!("RPC getblockhash({block_height}): {e}"))?;

    // 2. Fetch the full block (verbosity 3 includes prevout data when available)
    let block_json: serde_json::Value = rpc
        .call("getblock", &[json!(block_hash.to_string()), json!(3)])
        .map_err(|e| format!("RPC getblock({block_hash}): {e}"))?;

    let txs = block_json["tx"]
        .as_array()
        .ok_or_else(|| format!("RPC getblock: no tx array in block #{block_height}"))?;

    for tx_json in txs {
        let txid_str = match tx_json["txid"].as_str() {
            Some(s) => s,
            None => continue,
        };
        let txid_bytes =
            hex::decode(txid_str).map_err(|e| format!("bad txid hex '{txid_str}': {e}"))?;
        if txid_bytes.len() < 8 {
            continue;
        }
        // Match by 8-byte prefix (display byte order, same as get_8_hash_bytes)
        let prefix: [u8; 8] = txid_bytes[..8].try_into().unwrap();
        if prefix != target_prefix {
            continue;
        }

        // Found the tx — build TransactionBytesCursor
        let mut inputs = Vec::new();
        if let Some(vins) = tx_json["vin"].as_array() {
            for vin in vins {
                // Skip coinbase inputs
                if vin.get("coinbase").and_then(|v| v.as_str()).is_some() {
                    continue;
                }
                let input_txid_str = vin["txid"]
                    .as_str()
                    .ok_or_else(|| "missing vin txid".to_string())?;
                let input_txid_bytes =
                    hex::decode(input_txid_str).map_err(|e| format!("bad input txid hex: {e}"))?;
                let txin: [u8; 8] = input_txid_bytes[..8]
                    .try_into()
                    .map_err(|_| "input txid too short".to_string())?;
                let vout_idx = vin["vout"]
                    .as_u64()
                    .ok_or_else(|| "missing vout".to_string())?
                    as u16;

                // Prefer prevout from verbosity=3 response; fall back to separate RPC calls
                let (txin_value, input_block_height) = if let Some(prevout) = vin.get("prevout") {
                    let value_sat = prevout["value"]
                        .as_f64()
                        .map(|v| (v * 1e8).round() as u64)
                        .ok_or_else(|| "missing prevout.value".to_string())?;
                    let height = prevout["height"]
                        .as_u64()
                        .ok_or_else(|| "missing prevout.height".to_string())?
                        as u32;
                    (value_sat, height)
                } else {
                    // Dogecoin Core omitted prevout — fetch parent tx separately
                    let parent: serde_json::Value = rpc
                        .call("getrawtransaction", &[json!(input_txid_str), json!(true)])
                        .map_err(|e| format!("RPC getrawtransaction({input_txid_str}): {e}"))?;
                    let value_sat = parent["vout"][vout_idx as usize]["value"]
                        .as_f64()
                        .map(|v| (v * 1e8).round() as u64)
                        .ok_or_else(|| format!("missing parent vout[{vout_idx}].value"))?;
                    let blockhash_str = parent["blockhash"]
                        .as_str()
                        .ok_or_else(|| "missing parent tx blockhash".to_string())?;
                    let header: serde_json::Value = rpc
                        .call("getblockheader", &[json!(blockhash_str), json!(true)])
                        .map_err(|e| format!("RPC getblockheader({blockhash_str}): {e}"))?;
                    let height = header["height"]
                        .as_u64()
                        .ok_or_else(|| "missing header height".to_string())?
                        as u32;
                    (value_sat, height)
                };

                inputs.push(TransactionInputBytesCursor {
                    txin,
                    block_height: input_block_height,
                    vout: vout_idx,
                    txin_value,
                });
            }
        }

        let outputs: Vec<u64> = tx_json["vout"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|v| v["value"].as_f64().map(|f| (f * 1e8).round() as u64))
            .collect();

        return Ok(TransactionBytesCursor {
            txid: prefix,
            inputs,
            outputs,
        });
    }

    Err(format!(
        "tx {} not found in block #{block_height} via RPC",
        hex::encode(target_prefix)
    ))
}

#[cfg(test)]
mod test {
    use std::{hash::BuildHasherDefault, sync::Arc};

    use config::Config;
    use dashmap::DashMap;
    use dogecoin::types::dogecoin::TxOut;
    use dogecoin::{
        types::{
            BlockIdentifier, TransactionBytesCursor, TransactionIdentifier,
            TransactionInputBytesCursor,
        },
        utils::Context,
    };
    use fxhash::FxHasher;

    use super::compute_koinu_number;
    use crate::{
        core::{
            new_traversals_lazy_cache,
            test_builders::{TestBlockBuilder, TestTransactionBuilder, TestTxInBuilder},
        },
        db::{
            blocks::{insert_standardized_block, open_blocks_db_with_retry},
            drop_all_dbs,
        },
    };

    fn store_tx_in_traversals_cache(
        cache: &DashMap<(u32, [u8; 8]), TransactionBytesCursor, BuildHasherDefault<FxHasher>>,
        block_height: u64,
        block_hash: String,
        tx_hash: String,
        input_tx_hash: String,
        input_value: u64,
        input_vout: u16,
        output_value: u64,
    ) {
        let block_identifier = BlockIdentifier {
            index: block_height,
            hash: block_hash,
        };
        let transaction_identifier = TransactionIdentifier {
            hash: tx_hash.clone(),
        };
        let txid = transaction_identifier.get_8_hash_bytes();
        cache.insert(
            (block_identifier.index as u32, txid.clone()),
            TransactionBytesCursor {
                txid,
                inputs: vec![TransactionInputBytesCursor {
                    txin: (TransactionIdentifier {
                        hash: input_tx_hash,
                    })
                    .get_8_hash_bytes(),
                    block_height: (block_height - 1) as u32,
                    vout: input_vout,
                    txin_value: input_value,
                }],
                outputs: vec![output_value],
            },
        );
    }

    #[test]
    fn compute_sat_with_cached_traversals() {
        let ctx = Context::empty();
        let config = Config::test_default();
        drop_all_dbs(&config);
        let blocks_db = open_blocks_db_with_retry(true, &config, &ctx);
        let cache = new_traversals_lazy_cache(100);

        // Make cache contain the tx input trace (850000 -> 849999 -> 849998) so it doesn't have to visit rocksdb in every step.
        store_tx_in_traversals_cache(
            &cache,
            850000,
            "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187".to_string(),
            "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string(),
            "0xa321c61c83563a377f82ef59301f2527079f6bda7c2d04f9f5954c873f42e8ac".to_string(),
            9_000,
            0,
            8_000,
        );
        store_tx_in_traversals_cache(
            &cache,
            849999,
            "0x000000000000000000026b072f9347d86942f6786dd1fc362acfd9522715b313".to_string(),
            "0xa321c61c83563a377f82ef59301f2527079f6bda7c2d04f9f5954c873f42e8ac".to_string(),
            "0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c".to_string(),
            10_000,
            0,
            9_000,
        );
        // Store the sat coinbase block only (849998), it's the only one we need to access from blocks DB.
        insert_standardized_block(
            &TestBlockBuilder::new()
                .height(849998)
                .hash(
                    "0x00000000000000000000ec8da633f1fb0f8f281e43c52e5702139fac4f91204a"
                        .to_string(),
                )
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c"
                                .to_string(),
                        )
                        .build(),
                )
                .build(),
            &blocks_db,
            &ctx,
        );

        let block_identifier = BlockIdentifier {
            index: 850000,
            hash: "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187".to_string(),
        };
        let transaction_identifier = TransactionIdentifier {
            hash: "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string(),
        };
        let Ok((result, pointer, _)) = compute_koinu_number(
            &block_identifier,
            &transaction_identifier,
            0,
            8_000,
            &Arc::new(cache),
            &blocks_db,
            &config,
            &ctx,
        ) else {
            panic!();
        };

        assert_eq!(result.doginal_number, 1971874375008000);
        assert_eq!(result.transfers, 2);
        assert_eq!(
            result.transaction_identifier_inscription.hash,
            "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string()
        );
        assert_eq!(pointer, 8000);
    }

    #[test]
    fn compute_sat_with_rocksdb_traversals() {
        let ctx = Context::empty();
        let config = Config::test_default();
        drop_all_dbs(&config);
        let blocks_db = open_blocks_db_with_retry(true, &config, &ctx);
        let cache = new_traversals_lazy_cache(100);

        // Insert blocks directly into rocksdb to test non-cached lookups.
        insert_standardized_block(
            &TestBlockBuilder::new()
                .height(850000)
                .hash(
                    "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187"
                        .to_string(),
                )
                .add_transaction(TestTransactionBuilder::new().build())
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9"
                                .to_string(),
                        )
                        .add_input(
                            TestTxInBuilder::new()
                                .prev_out_block_height(849999)
                                .prev_out_tx_hash("0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c".to_string())
                                .value(10_000)
                                .build(),
                        )
                        .build(),
                )
                .build(),
            &blocks_db,
            &ctx,
        );
        insert_standardized_block(
            &TestBlockBuilder::new()
                .height(849999)
                .hash(
                    "0x00000000000000000000ec8da633f1fb0f8f281e43c52e5702139fac4f91204a"
                        .to_string(),
                )
                .add_transaction(TestTransactionBuilder::new().build())
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c"
                                .to_string(),
                        )
                        .add_input(
                            TestTxInBuilder::new()
                                .prev_out_block_height(849998)
                                .prev_out_tx_hash("0xcf90b73725382d7485868379b166cfd0614d507b547ea2af8a13cd6c6b7e837f".to_string())
                                .value(12_000)
                                .build(),
                        )
                        .add_output(TxOut {
                            value: 10_000,
                            script_pubkey: "76a914fb37342f6275b13936799def06f2eb4c0f20151588ac".to_string(),
                        })
                        .build(),
                )
                .build(),
            &blocks_db,
            &ctx,
        );
        insert_standardized_block(
            &TestBlockBuilder::new()
                .height(849998)
                .hash(
                    "0x000000000000000000030b3451d402089d510234c665d130ebc3e8a1355633a0"
                        .to_string(),
                )
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xcf90b73725382d7485868379b166cfd0614d507b547ea2af8a13cd6c6b7e837f"
                                .to_string(),
                        )
                        .add_output(TxOut {
                            value: 12_000,
                            script_pubkey: "".to_string(),
                        })
                        .build(),
                )
                .build(),
            &blocks_db,
            &ctx,
        );

        let block_identifier = BlockIdentifier {
            index: 850000,
            hash: "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187".to_string(),
        };
        let transaction_identifier = TransactionIdentifier {
            hash: "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string(),
        };
        let Ok((result, pointer, _)) = compute_koinu_number(
            &block_identifier,
            &transaction_identifier,
            0,
            8_000,
            &Arc::new(cache),
            &blocks_db,
            &config,
            &ctx,
        ) else {
            panic!();
        };

        assert_eq!(result.doginal_number, 1971874375008000);
        assert_eq!(result.transfers, 2);
        assert_eq!(
            result.transaction_identifier_inscription.hash,
            "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string()
        );
        assert_eq!(pointer, 8000);
    }

    #[test]
    fn compute_sat_from_coinbase_fees() {
        let ctx = Context::empty();
        let config = Config::test_default();
        drop_all_dbs(&config);
        let blocks_db = open_blocks_db_with_retry(true, &config, &ctx);
        let cache = new_traversals_lazy_cache(100);

        // Insert blocks such that we land on a coinbase transaction but the inscription sat actually comes from a fee paid by
        // another tx in that block.
        store_tx_in_traversals_cache(
            &cache,
            850000,
            "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187".to_string(),
            "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string(),
            "0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c".to_string(),
            9_000,
            1,
            8_000,
        );
        insert_standardized_block(
            &TestBlockBuilder::new()
                .height(849999)
                .hash(
                    "0x00000000000000000000ec8da633f1fb0f8f281e43c52e5702139fac4f91204a"
                        .to_string(),
                )
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c"
                                .to_string(),
                        )
                        .add_output(TxOut {
                            value: 312_500_000, // Full block subsidy.
                            script_pubkey: "".to_string(),
                        })
                        .build(),
                )
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xfc47db8141ec6a74ee643b839aa73a44619c90d9000621f8752be1f875f2298f"
                                .to_string(),
                        )
                        // The fees will come from a previous block.
                        .add_input(
                            TestTxInBuilder::new()
                                .prev_out_block_height(849998)
                                .prev_out_tx_hash("0xcf90b73725382d7485868379b166cfd0614d507b547ea2af8a13cd6c6b7e837f".to_string())
                                .value(20_000)
                                .build(),
                        )
                        .add_output(TxOut {
                            value: 9_000, // This makes fees spent to be where the inscription goes.
                            script_pubkey: "0x76a914fb37342f6275b13936799def06f2eb4c0f20151588ac"
                                .to_string(),
                        })
                        .build(),
                )
                .build(),
            &blocks_db,
            &ctx,
        );
        insert_standardized_block(
            &TestBlockBuilder::new()
                .height(849998)
                .hash(
                    "0x000000000000000000030b3451d402089d510234c665d130ebc3e8a1355633a0"
                        .to_string(),
                )
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xcf90b73725382d7485868379b166cfd0614d507b547ea2af8a13cd6c6b7e837f"
                                .to_string(),
                        )
                        .add_output(TxOut {
                            value: 20_000,
                            script_pubkey: "".to_string(),
                        })
                        .build(),
                )
                .build(),
            &blocks_db,
            &ctx,
        );

        let block_identifier = BlockIdentifier {
            index: 850000,
            hash: "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187".to_string(),
        };
        let transaction_identifier = TransactionIdentifier {
            hash: "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string(),
        };
        let Ok((result, pointer, _)) = compute_koinu_number(
            &block_identifier,
            &transaction_identifier,
            0,
            8_000,
            &Arc::new(cache),
            &blocks_db,
            &config,
            &ctx,
        ) else {
            panic!();
        };

        assert_eq!(result.doginal_number, 1971874375017000);
        assert_eq!(result.transfers, 2);
        assert_eq!(
            result.transaction_identifier_inscription.hash,
            "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string()
        );
        assert_eq!(pointer, 8000);
    }

    #[test]
    fn compute_sat_from_non_spend_cached_transactions() {
        let ctx = Context::empty();
        let config = Config::test_default();
        drop_all_dbs(&config);
        let blocks_db = open_blocks_db_with_retry(true, &config, &ctx);
        let cache = new_traversals_lazy_cache(100);

        store_tx_in_traversals_cache(
            &cache,
            850000,
            "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187".to_string(),
            "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string(),
            "0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c".to_string(),
            9_000,
            0,
            8_000,
        );
        store_tx_in_traversals_cache(
            &cache,
            849999,
            "0x00000000000000000000ec8da633f1fb0f8f281e43c52e5702139fac4f91204a".to_string(),
            "0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c".to_string(),
            "0xfc47db8141ec6a74ee643b839aa73a44619c90d9000621f8752be1f875f2298f".to_string(),
            0, // Non spend
            1,
            8_000,
        );

        let block_identifier = BlockIdentifier {
            index: 850000,
            hash: "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187".to_string(),
        };
        let transaction_identifier = TransactionIdentifier {
            hash: "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string(),
        };
        let Ok((result, _, _)) = compute_koinu_number(
            &block_identifier,
            &transaction_identifier,
            0,
            8_000,
            &Arc::new(cache),
            &blocks_db,
            &config,
            &ctx,
        ) else {
            panic!();
        };

        assert_eq!(result.doginal_number, 0);
        assert_eq!(result.transfers, 0);
    }

    #[test]
    fn compute_sat_from_non_spend_rocksdb_traversals() {
        let ctx = Context::empty();
        let config = Config::test_default();
        drop_all_dbs(&config);
        let blocks_db = open_blocks_db_with_retry(true, &config, &ctx);
        let cache = new_traversals_lazy_cache(100);

        insert_standardized_block(
            &TestBlockBuilder::new()
                .height(850000)
                .hash(
                    "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187"
                        .to_string(),
                )
                .add_transaction(TestTransactionBuilder::new().build())
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9"
                                .to_string(),
                        )
                        .add_input(
                            TestTxInBuilder::new()
                                .prev_out_block_height(849999)
                                .prev_out_tx_hash("0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c".to_string())
                                .value(10_000)
                                .build(),
                        )
                        .build(),
                )
                .build(),
            &blocks_db,
            &ctx,
        );
        insert_standardized_block(
            &TestBlockBuilder::new()
                .height(849999)
                .hash(
                    "0x00000000000000000000ec8da633f1fb0f8f281e43c52e5702139fac4f91204a"
                        .to_string(),
                )
                .add_transaction(TestTransactionBuilder::new().build())
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xa077643d3411362c9f75377a832aee6666c73b4358ebccf98f6dad82e57bbe1c"
                                .to_string(),
                        )
                        .add_input(
                            TestTxInBuilder::new()
                                .prev_out_block_height(849998)
                                .prev_out_tx_hash("0xcf90b73725382d7485868379b166cfd0614d507b547ea2af8a13cd6c6b7e837f".to_string())
                                .value(0) // Non spend
                                .build(),
                        )
                        .add_output(TxOut {
                            value: 10_000,
                            script_pubkey: "0x76a914fb37342f6275b13936799def06f2eb4c0f20151588ac"
                                .to_string(),
                        })
                        .build(),
                )
                .build(),
            &blocks_db,
            &ctx,
        );
        insert_standardized_block(
            &TestBlockBuilder::new()
                .height(849998)
                .hash(
                    "0x000000000000000000030b3451d402089d510234c665d130ebc3e8a1355633a0"
                        .to_string(),
                )
                .add_transaction(
                    TestTransactionBuilder::new()
                        .hash(
                            "0xcf90b73725382d7485868379b166cfd0614d507b547ea2af8a13cd6c6b7e837f"
                                .to_string(),
                        )
                        .add_output(TxOut {
                            value: 12_000,
                            script_pubkey: "".to_string(),
                        })
                        .build(),
                )
                .build(),
            &blocks_db,
            &ctx,
        );

        let block_identifier = BlockIdentifier {
            index: 850000,
            hash: "0x00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187".to_string(),
        };
        let transaction_identifier = TransactionIdentifier {
            hash: "0xc62d436323e14cdcb91dd21cb7814fd1ac5b9ecb6e3cc6953b54c02a343f7ec9".to_string(),
        };
        let Ok((result, _, _)) = compute_koinu_number(
            &block_identifier,
            &transaction_identifier,
            0,
            8_000,
            &Arc::new(cache),
            &blocks_db,
            &config,
            &ctx,
        ) else {
            panic!();
        };

        assert_eq!(result.doginal_number, 0);
        assert_eq!(result.transfers, 0);
    }
}
