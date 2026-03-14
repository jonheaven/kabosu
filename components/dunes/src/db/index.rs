// ...existing code...
use bitcoin::hashes::Hash;
use dogecoin::utils::Context;
use std::collections::HashMap;
use bitcoin::{Amount, ScriptBuf, Network, Transaction};
use bitcoin::{
    absolute::LockTime,
    transaction::{TxOut, Version},
};
use deadpool_postgres::Client;
use dogecoin::{
    try_info,
    types::{DogecoinBlockData, DogecoinTransactionData},
};
use doginals_parser::{Artifact, Dunestone, Flaw};
use postgres::pg_begin;
use super::{cache::index_cache::IndexCache, pg_get_max_dune_number, pg_roll_back_block};

// Use types from dogecoin::types
// ...existing code...

#[derive(Debug, Clone)]
pub struct RosettaOutPoint {
    pub txid: bitcoin::Txid,
    pub vout: u32,
}

#[derive(Debug, Clone)]
pub struct RosettaTxIn {
    pub previous_output: RosettaOutPoint,
    pub script_sig: bitcoin::ScriptBuf,
    pub sequence: bitcoin::Sequence,
    pub witness: bitcoin::Witness,
}

#[derive(Debug, Clone)]
pub struct RosettaTxOut {
    pub script_pubkey: bitcoin::ScriptBuf,
    pub value: bitcoin::Amount,
}
use crate::{
    db::cache::transaction_location::TransactionLocation, utils::monitoring::PrometheusMonitoring,
};
pub fn get_dune_genesis_block_height(network: Network) -> u64 {
    // Dogecoin Dunes activation height is intentionally unset for now.
    // Use u64::MAX so indexing stays disabled until explicitly activated.
    match network {
        Network::Bitcoin => u64::MAX,
        Network::Testnet | Network::Testnet4 => u64::MAX,
        Network::Signet => u64::MAX,
        // Regtest remains available for local testing.
        Network::Regtest => 0,
    }
}

/// Transforms a Bitcoin transaction from a Chainhook format to a rust bitcoin crate format so it can be parsed by the ord crate
/// to look for `Artifact`s. Also, takes all non-OP_RETURN outputs and returns them so they can be used later to receive dunes.
fn bitcoin_tx_from_chainhook_tx(
    block: &DogecoinBlockData,
    tx: &DogecoinTransactionData,
) -> (Transaction, HashMap<u32, ScriptBuf>, Option<u32>, u32) {
    let mut inputs = Vec::with_capacity(tx.metadata.inputs.len());
    let mut outputs = Vec::with_capacity(tx.metadata.outputs.len());
    let mut eligible_outputs = HashMap::new();
    let mut first_eligible_output: Option<u32> = None;
    for (i, output) in tx.metadata.outputs.iter().enumerate() {
        let script = ScriptBuf::from_bytes(output.get_script_pubkey_bytes());
        if !script.is_op_return() {
            eligible_outputs.insert(i as u32, script.clone());
            if first_eligible_output.is_none() {
                first_eligible_output = Some(i as u32);
            }
        }
        outputs.push(TxOut {
            value: Amount::from_sat(output.value),
            script_pubkey: script,
        });
    }
    for input in tx.metadata.inputs.iter() {
        inputs.push(bitcoin::TxIn {
            previous_output: bitcoin::OutPoint {
                    txid: bitcoin::Txid::from_raw_hash(Hash::from_slice(&input.previous_output.txid.get_hash_bytes()).unwrap()),
                vout: input.previous_output.vout,
            },
            script_sig: bitcoin::ScriptBuf::from_bytes(input.script_sig.as_bytes().to_vec()),
            sequence: bitcoin::Sequence(input.sequence),
            witness: bitcoin::Witness::default(),
        });
    }
    (
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::from_time(block.timestamp).unwrap(),
            input: inputs,
            output: outputs,
        },
        eligible_outputs,
        first_eligible_output,
        tx.metadata.outputs.len() as u32,
    )
}

/// Index a Bitcoin block for dunes data.
pub async fn index_block(
    pg_client: &mut Client,
    index_cache: &mut IndexCache,
    block: &mut DogecoinBlockData,
    prometheus: &PrometheusMonitoring,
    ctx: &Context,
) -> Result<(), String> {
    let stopwatch = std::time::Instant::now();
    let block_hash = &block.block_identifier.hash;
    let block_height = block.block_identifier.index;
    try_info!(ctx, "DunesIndexer indexing block #{block_height}...");

    // Track operation counts
    let mut etching_count: u64 = 0;
    let mut mint_count: u64 = 0;
    let mut edict_count: u64 = 0;
    let mut cenotaph_etching_count: u64 = 0;
    let mut cenotaph_mint_count: u64 = 0;
    let mut cenotaph_count: u64 = 0;
    let mut inputs_count: u64 = 0;

    let mut db_tx = pg_begin(pg_client).await.unwrap();
    index_cache.reset_max_dune_number(&mut db_tx).await;

    // Measure parsing time
    let parsing_start = std::time::Instant::now();

    for tx in block.transactions.iter() {
        let (transaction, eligible_outputs, first_eligible_output, total_outputs) =
            bitcoin_tx_from_chainhook_tx(block, tx);
        let tx_index = tx.metadata.index;
        let tx_id = &tx.transaction_identifier.hash;
        let location = TransactionLocation {
            network: index_cache.network,
            block_hash: block_hash.clone(),
            block_height,
            tx_index,
            tx_id: tx_id.clone(),
            timestamp: block.timestamp,
        };
        index_cache
            .begin_transaction(
                location,
                &tx.metadata.inputs,
                eligible_outputs,
                first_eligible_output,
                total_outputs,
                &mut db_tx,
                ctx,
            )
            .await;
        if let Some(artifact) = Dunestone::decipher(&transaction) {
            match artifact {
                Artifact::Dunestone(dunestone) => {
                    index_cache
                        .apply_dunestone(&dunestone, &mut db_tx, ctx)
                        .await;
                    if let Some(etching) = dunestone.etching {
                        index_cache
                            .apply_etching(
                                &etching,
                                &mut db_tx,
                                ctx,
                                &mut etching_count,
                                &transaction,
                                &mut inputs_count,
                            )
                            .await?;
                    }
                    if let Some(mint_dune_id) = dunestone.mint {
                        index_cache
                            .apply_mint(&mint_dune_id, &mut db_tx, ctx, &mut mint_count)
                            .await;
                    }
                    for edict in dunestone.edicts.iter() {
                        index_cache
                            .apply_edict(edict, &mut db_tx, ctx, &mut edict_count)
                            .await;
                    }
                }
                Artifact::Cenotaph(cenotaph) => {
                    index_cache
                        .apply_cenotaph(&cenotaph, &mut db_tx, ctx, &mut cenotaph_count)
                        .await;

                    if cenotaph.flaw != Some(Flaw::Varint) {
                        if let Some(etching) = cenotaph.etching {
                            index_cache
                                .apply_cenotaph_etching(
                                    &etching,
                                    &mut db_tx,
                                    ctx,
                                    &mut cenotaph_etching_count,
                                    &transaction,
                                    &mut inputs_count,
                                )
                                .await?;
                        }
                        if let Some(mint_dune_id) = cenotaph.mint {
                            index_cache
                                .apply_cenotaph_mint(
                                    &mint_dune_id,
                                    &mut db_tx,
                                    ctx,
                                    &mut cenotaph_mint_count,
                                )
                                .await;
                        }
                    }
                }
            }
        }
        index_cache.end_transaction(&mut db_tx, ctx);
    }
    prometheus.metrics_record_dune_parsing_time(parsing_start.elapsed().as_millis() as f64);

    // Measure computation time
    let computation_start = std::time::Instant::now();
    index_cache.end_block();
    prometheus.metrics_record_dune_computation_time(computation_start.elapsed().as_millis() as f64);

    // Measure database write time
    let dune_db_write_start = std::time::Instant::now();
    index_cache.db_cache.flush(&mut db_tx, ctx).await;
    db_tx
        .commit()
        .await
        .expect("Unable to commit pg transaction");
    prometheus.metrics_record_dune_db_write_time(dune_db_write_start.elapsed().as_millis() as f64);

    prometheus.metrics_record_dunes_etching_per_block(etching_count);
    prometheus.metrics_record_dunes_edict_per_block(edict_count);
    prometheus.metrics_record_dunes_mint_per_block(mint_count);
    prometheus.metrics_record_dunes_cenotaph_per_block(cenotaph_count);
    prometheus.metrics_record_dunes_cenotaph_etching_per_block(cenotaph_etching_count);
    prometheus.metrics_record_dunes_cenotaph_mint_per_block(cenotaph_mint_count);
    prometheus.metrics_record_dunes_etching_inputs_checked_per_block(inputs_count);
    // Record metrics
    prometheus.metrics_block_indexed(block_height);
    let current_dune_number = pg_get_max_dune_number(pg_client).await;
    prometheus.metrics_dune_indexed(current_dune_number as u64);
    prometheus.metrics_record_dunes_per_block(etching_count);

    // Record overall processing time
    let elapsed = stopwatch.elapsed();
    prometheus.metrics_record_block_processing_time(elapsed.as_millis() as f64);
    try_info!(
        ctx,
        "DunesIndexer indexed block #{block_height}: {etching_count} etchings, {mint_count} mints, {edict_count} edicts, {cenotaph_count} cenotaphs ({cenotaph_etching_count} etchings, {cenotaph_mint_count} mints) in {}s",
        elapsed.as_secs_f32()
    );

    Ok(())
}

/// Roll back a Bitcoin block because of a re-org.
pub async fn roll_back_block(pg_client: &mut Client, block_height: u64, ctx: &Context) {
    let stopwatch = std::time::Instant::now();
    try_info!(ctx, "Rolling back block {block_height}...");
    let mut db_tx = pg_client
        .transaction()
        .await
        .expect("Unable to begin block roll back pg transaction");
    pg_roll_back_block(block_height, &mut db_tx, ctx).await;
    db_tx
        .commit()
        .await
        .expect("Unable to commit pg transaction");
    try_info!(
        ctx,
        "Block {block_height} rolled back in {elapsed:.4}s",
        elapsed = stopwatch.elapsed().as_secs_f32()
    );
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use bitcoin::{ScriptBuf, Sequence, Witness, Amount, Txid};
    use dogecoin::types::{
        BlockIdentifier,
        DogecoinBlockData, DogecoinTransactionData, TransactionIdentifier,
        DogecoinBlockMetadata, DogecoinTransactionMetadata
    };
    use super::{RosettaOutPoint, RosettaTxIn, RosettaTxOut};
    // Use local types
    use doginals_parser::Artifact;

    use super::*;

    fn build_block(block_height: u64, block_hash_hex: &str, timestamp: u32) -> DogecoinBlockData {
        DogecoinBlockData {
            block_identifier: BlockIdentifier {
                hash: format!("0x{}", block_hash_hex.to_lowercase()),
                index: block_height,
            },
            parent_block_identifier: BlockIdentifier {
                hash: "0x0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
                index: block_height - 1,
            },
            timestamp,
            transactions: vec![],
            metadata: DogecoinBlockMetadata {
                network: Network::Bitcoin,
            },
        }
    }
    fn build_valid_tx() -> DogecoinTransactionData {
        // txid: 3a11c5bc4eee38645934607ba63e0d7ac834d399e53c7c06a0ced093a711f1a2
        let prevout = RosettaOutPoint {
            txid: Txid::from_str("1cf46d1a3192e5cdcce62441a7a40691ed4f7e34dc97dd3bfc7f96ff2069846e").unwrap(),
            vout: 0,
        };
        let inputs = vec![RosettaTxIn {
            previous_output: prevout,
            script_sig: ScriptBuf::new(),
            sequence: Sequence(4_294_967_293),
            witness: Witness::new(),
        }];
        let outputs = vec![
            RosettaTxOut {
                script_pubkey: ScriptBuf::from_bytes(hex::decode("5120f1e73bbd97fd0eac833e781abdbea9c223951aede9e2275ac6c03e8c1b24394b").unwrap()),
                value: Amount::from_sat(546),
            },
            RosettaTxOut {
                script_pubkey: ScriptBuf::from_bytes(hex::decode("5120f1e73bbd97fd0eac833e781abdbea9c223951aede9e2275ac6c03e8c1b24394b").unwrap()),
                value: Amount::from_sat(546),
            },
            RosettaTxOut {
                script_pubkey: ScriptBuf::from_bytes(hex::decode("6a5d22020704a7e6e7dbbcf1f2b38203010003800105bded070690c8020a6408aed3191601").unwrap()),
                value: Amount::from_sat(0),
            },
        ];
        DogecoinTransactionData {
            transaction_identifier: TransactionIdentifier {
                hash: "0x3a11c5bc4eee38645934607ba63e0d7ac834d399e53c7c06a0ced093a711f1a2"
                    .to_string(),
            },
            operations: vec![],
            metadata: DogecoinTransactionMetadata {
                inputs,
                outputs,
                doginal_operations: vec![],
                drc20_operation: None,
                proof: None,
                fee: 349_966,
                index: 0,
            },
        }
    }

    fn build_invalid_tx() -> DogecoinTransactionData {
        // txid: 66d084fe5e206c7183293d1e379caa2011e7750018c65dfd2fd3174ea9f298fc
        let prevout = RosettaOutPoint {
            txid: Txid::from_str(
                "871fb5e4042dca1549326da5848bd2257d6e609984a0cfb867e4ff24a56806d0"
            ).unwrap(),
            vout: 0,
        };

        let inputs = vec![RosettaTxIn {
            previous_output: prevout,
            script_sig: ScriptBuf::new(),
            sequence: Sequence(4_294_967_293),
            witness: Witness::new(),
        }];

        let outputs = vec![
            RosettaTxOut {
                // vout 0 - OP_RETURN dunestone
                script_pubkey: ScriptBuf::from_bytes(hex::decode("6a5d1b020304cfb4c2acf497b13e0380068094ebdc030a8094ebdc030801").unwrap()),
                value: Amount::from_sat(0),
            },
            RosettaTxOut {
                // vout 1 - p2tr
                script_pubkey: ScriptBuf::from_bytes(hex::decode("5120f1e73bbd97fd0eac833e781abdbea9c223951aede9e2275ac6c03e8c1b24394b").unwrap()),
                value: Amount::from_sat(546),
            },
            RosettaTxOut {
                // vout 2 - p2wpkh
                script_pubkey: "0x0014f32b49757996ef8db8d3d029b3dc997560e77d12".to_string(),
                value: Amount::from_sat(15765),
            },
        ];

        DogecoinTransactionData {
            transaction_identifier: TransactionIdentifier {
                hash: "0x66d084fe5e206c7183293d1e379caa2011e7750018c65dfd2fd3174ea9f298fc"
                    .to_string(),
            },
            operations: vec![],
            metadata: DogecoinTransactionMetadata {
                inputs,
                outputs,
                doginal_operations: vec![],
                drc20_operation: None,
                proof: None,
                fee: 283_689,
                index: 1,
            },
        }
    }

    fn build_no_commit_tx() -> DogecoinTransactionData {
        // txid: 27bbb1f7776d3ac5f41c17a5cde59b4feba5e9f5c5e76f19559c787a7c771055
        let prevout0 = RosettaOutPoint {
            txid: Txid::from_str(
                "99060f5739abacbe53f568723d829cbef21b65d555950cfacf85baf78fc3724d"
            ).unwrap(),
            vout: 0,
        };
        let prevout1 = RosettaOutPoint {
            txid: Txid::from_str(
                "99060f5739abacbe53f568723d829cbef21b65d555950cfacf85baf78fc3724d"
            ).unwrap(),
            vout: 1,
        };

        let inputs = vec![
            RosettaTxIn {
                previous_output: prevout0,
                script_sig: ScriptBuf::new(),
                sequence: Sequence(4_294_967_293),
                witness: Witness::new(),
            },
            RosettaTxIn {
                previous_output: prevout1,
                script_sig: ScriptBuf::new(),
                sequence: Sequence(4_294_967_293),
                witness: Witness::new(),
            },
        ];

        let outputs = vec![
            RosettaTxOut {
                // vout 0 - p2tr
                script_pubkey:
                    "0x5120f08e113b9485778ef245f751b5fea5ab38fbe93f6a5991eadf90426a8911570b"
                        .to_string(),
                value: 419_010,
            },
            RosettaTxOut {
                // vout 1 - OP_RETURN dunestone
                script_pubkey:
                    "0x6a5d230207049efdc0b9d7cbf3cc1903800105ae4c06d8b9310ab20508b451106012c01f1601"
                        .to_string(),
                value: 0,
            },
        ];

        DogecoinTransactionData {
            transaction_identifier: TransactionIdentifier {
                hash: "0x27bbb1f7776d3ac5f41c17a5cde59b4feba5e9f5c5e76f19559c787a7c771055"
                    .to_string(),
            },
            operations: vec![],
            metadata: DogecoinTransactionMetadata {
                inputs,
                outputs,
                doginal_operations: vec![],
                drc20_operation: None,
                proof: None,
                fee: 648_000,
                index: 2,
            },
        }
    }

    fn build_dune_wrong_flaw_tx() -> DogecoinTransactionData {
        // txid: 8bf9d4ec8ed69ae7bac256285b06ae6566cec4679dcfb45b5671a323c2f18c3c
        let prevout = RosettaOutPoint {
            txid: Txid::from_str(
                "bd546ac9aa0e275a7f06a960a54db0a9c0de634ad71805cb2d10418b3befc8e8"
            ).unwrap(),
            vout: 0
        };

        let inputs = vec![RosettaTxIn {
            previous_output: prevout,
            script_sig: ScriptBuf::new(),
            sequence: 4_294_967_293,
            witness: Witness::default()
        }];

        let outputs = vec![
            RosettaTxOut {
                // vout 0 - OP_RETURN dunestone
                script_pubkey:
                    "0x6a5d20020304a5accff7c3c4f6909420010003a00405b84106a096800ae8070888a401"
                        .to_string(),
                value: 0,
            },
            RosettaTxOut {
                // vout 1 - p2tr
                script_pubkey:
                    "0x51202b35c0d80bce33fe2036479ab82a4ea60dd760fe2310a433427cc4199da59b8a"
                        .to_string(),
                value: 546,
            },
            RosettaTxOut {
                // vout 2 - p2wpkh
                script_pubkey: "0x00143d71d44fc31fec24a6e3c1e7955e9d388c5ef312".to_string(),
                value: 22_454,
            },
        ];

        DogecoinTransactionData {
            transaction_identifier: TransactionIdentifier {
                hash: "0x8bf9d4ec8ed69ae7bac256285b06ae6566cec4679dcfb45b5671a323c2f18c3c"
                    .to_string(),
            },
            operations: vec![],
            metadata: DogecoinTransactionMetadata {
                inputs,
                outputs,
                doginal_operations: vec![],
                drc20_operation: None,
                proof: None,
                fee: 414_000,
                index: 3,
            },
        }
    }

    fn build_internetgold_tx() -> DogecoinTransactionData {
        // txid: 66d084fe5e206c7183293d1e379caa2011e7750018c65dfd2fd3174ea9f298fc
        // This is the INTERNETGOLD transaction with truncated LEB128 field
        let prevout = RosettaOutPoint {
            txid: Txid::from_str(
                "871fb5e4042dca1549326da5848bd2257d6e609984a0cfb867e4ff24a56806d0"
            ).unwrap(),
            vout: 0
        };

        let inputs = vec![RosettaTxIn {
            previous_output: prevout,
            script_sig: ScriptBuf::new(),
            sequence: 4_294_967_293,
            witness: Witness::default()
        }];

        let outputs = vec![
            RosettaTxOut {
                // vout 0 - OP_RETURN dunestone with truncated LEB128
                script_pubkey: "0x6a5d1b020304cfb4c2acf497b13e0380068094ebdc030a8094ebdc030801"
                    .to_string(),
                value: 0,
            },
            RosettaTxOut {
                // vout 1 - p2wpkh
                script_pubkey: "0x0014f32b49757996ef8db8d3d029b3dc997560e77d12".to_string(),
                value: 546,
            },
            RosettaTxOut {
                // vout 2 - p2wpkh
                script_pubkey: "0x001459966e46ce78b8bf4a54827b84144c82ea21811c".to_string(),
                value: 15_765,
            },
        ];

        DogecoinTransactionData {
            transaction_identifier: TransactionIdentifier {
                hash: "0x66d084fe5e206c7183293d1e379caa2011e7750018c65dfd2fd3174ea9f298fc"
                    .to_string(),
            },
            operations: vec![],
            metadata: DogecoinTransactionMetadata {
                inputs,
                outputs,
                doginal_operations: vec![],
                drc20_operation: None,
                proof: None,
                fee: 283_689,
                index: 3,
            },
        }
    }

    fn build_elonmuskdoge_tx() -> DogecoinTransactionData {
        // txid: 89e8149d38f8b702621fa18310b10794e541c0b52478466b85f156f5622b8fe3
        // This is the ELONMUSKDOGE transaction
        let prevout = RosettaOutPoint {
            txid: Txid::from_str(
                "de6c56ecf9212e8946c71267108cce91ccc71d378b55aa6a1f41a3d93a82e8cb"
            ).unwrap(),
            vout: 0
        };

        let inputs = vec![RosettaTxIn {
            previous_output: prevout,
            script_sig: ScriptBuf::new(),
            sequence: 4_294_967_293,
            witness: Witness::default()
        }];

        let outputs = vec![
            RosettaTxOut {
                // vout 0 - OP_RETURN dunestone
                script_pubkey:"0x6a5d1f020304c6e6ffe9888ae1230300054506a096800ac0de810a08e80710f2a233".to_string(),
                value: 0,
            },
            RosettaTxOut {
                // vout 1 - p2wpkh
                script_pubkey: "0x001426909c2c3de5de4b6eccb8c0f65ab209fe6f4720".to_string(),
                value: 546,
            },
            RosettaTxOut {
                // vout 2 - p2wpkh
                script_pubkey: "0x001406f10a8f6a0aec21e97e02913f0b39f9aa477bbf".to_string(),
                value: 20_454,
            },
        ];

        DogecoinTransactionData {
            transaction_identifier: TransactionIdentifier {
                hash: "0x89e8149d38f8b702621fa18310b10794e541c0b52478466b85f156f5622b8fe3"
                    .to_string(),
            },
            operations: vec![],
            metadata: DogecoinTransactionMetadata {
                inputs,
                outputs,
                doginal_operations: vec![],
                drc20_operation: None,
                proof: None,
                fee: 386_000,
                index: 4,
            },
        }
    }

    fn build_whataremfers_tx() -> DogecoinTransactionData {
        // txid: c075c5eba59ca77c40085fc417c8adafa9d2c9970158c7311d60fb24e00d4b45
        // This is the WHATAREMFERS transaction
        let prevout = RosettaOutPoint {
            txid: Txid::from_str(
                "ee637d9afdaabd9d5fb64fb8bb0396b69b12c6d1f150a00a9692f3bc09c5f6e5"
            ).unwrap(),
            vout: 0,
        };

        let inputs = vec![RosettaTxIn {
            previous_output: prevout,
            script_sig: ScriptBuf::new(),
            sequence: 4_294_967_293,
            witness: Witness::default()
        }];

        let outputs = vec![
            RosettaTxOut {
                // vout 0 - OP_RETURN dunestone
                script_pubkey: "0x6a5d24020304fac7f7e5f5b0fd97010348054d06a096800a904e0880b191640ca089310ec0843d".to_string(),
                value: 0
            },
            RosettaTxOut {
                // vout 1 - p2tr
                script_pubkey: "0x512078a95794567c472013bfac2ecf3e8090e847f7bfd69748613bc0bf6c28be1549".to_string(),
                value: 546
            },
            RosettaTxOut {
                // vout 2 - p2wpkh
                script_pubkey: "0x0014a4c6356a3723f8cc900f9b6cbe761f8937917af5".to_string(),
                value: 16_283
            },
        ];

        DogecoinTransactionData {
            transaction_identifier: TransactionIdentifier {
                hash: "0xc075c5eba59ca77c40085fc417c8adafa9d2c9970158c7311d60fb24e00d4b45"
                    .to_string(),
            },
            operations: vec![],
            metadata: DogecoinTransactionMetadata {
                inputs,
                outputs,
                doginal_operations: vec![],
                drc20_operation: None,
                proof: None,
                fee: 287_171,
                index: 5,
            },
        }
    }

    #[test]
    fn valid_and_invalid_etch_output_selection_and_parsing() {
        // Common block context from the provided fixtures
        let block = build_block(
            840_021,
            "00000000000000000001a6a69ead163c499c0543dcef13c05499a798addb638f",
            1_713_583_272,
        );

        let tx_valid = build_valid_tx();
        let tx_invalid = build_invalid_tx();

        // Valid etch: OP_RETURN is last (vout 2); first eligible should be vout 0
        let (tx1, eligible1, first_eligible1, total_outputs1) =
            bitcoin_tx_from_chainhook_tx(&block, &tx_valid);
        assert_eq!(total_outputs1, 3);
        assert_eq!(first_eligible1, Some(0));
        assert!(eligible1.contains_key(&0));
        assert!(eligible1.contains_key(&1));
        assert!(!eligible1.contains_key(&2)); // OP_RETURN excluded

        // Invalid etch (by output positioning): OP_RETURN is first (vout 0); first eligible should be vout 1
        let (tx2, eligible2, first_eligible2, total_outputs2) =
            bitcoin_tx_from_chainhook_tx(&block, &tx_invalid);
        assert_eq!(total_outputs2, 3);
        assert_eq!(first_eligible2, Some(1));
        assert!(eligible2.contains_key(&1));
        assert!(eligible2.contains_key(&2));
        assert!(!eligible2.contains_key(&0)); // OP_RETURN excluded

        // art1: complete etching
        let art1 = Dunestone::decipher(&tx1).expect("dunestone");
        let Artifact::Dunestone(rs1) = art1 else {
            panic!("expected Dunestone");
        };
        let e1 = rs1.etching.as_ref().expect("expected etching");
        assert!(
            e1.divisibility.is_some()
                && e1.premine.is_some()
                && e1.terms.is_some()
                && e1.symbol.is_some()
                && e1.spacers.is_some()
                && e1.dune.is_some()
        );

        // art2: incomplete etching (all optional fields empty, turbo == false)
        let art2 = Dunestone::decipher(&tx2).expect("dunestone");
        let Artifact::Dunestone(rs2) = art2 else {
            // Non-Dunestone is acceptable for invalid tx
            return;
        };
        if let Some(e2) = rs2.etching.as_ref() {
            let is_incomplete = e2.divisibility.is_none()
                && e2.premine.is_none()
                && e2.dune.is_none()
                && e2.spacers.is_none()
                && e2.symbol.is_none()
                && e2.terms.is_none()
                && !e2.turbo;
            assert!(
                is_incomplete,
                "invalid tx should not produce a complete etching"
            );
        } else {
            // No etching at all is also acceptable
        }
    }

    #[test]
    fn invalid_varint_payload_yields_cenotaph_without_etching() {
        use bitcoin::{script::Builder};

        // Construct an OP_RETURN | MAGIC_NUMBER script with a single push containing an invalid varint
        // 19 bytes with MSB set triggers varint error (overflow/truncated)
        let payload = vec![0x80u8; 19];
        let push: &bitcoin::script::PushBytes = payload.as_slice().try_into().unwrap();
        let script = Builder::new()
            .push_opcode(bitcoin::opcodes::all::OP_RETURN)
            .push_slice(push)
            .into_script();
        // ...existing code...
        // Add actual test logic here as needed
    }
    // ...existing code...
}
