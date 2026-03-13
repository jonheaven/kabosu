use std::{collections::HashMap, str, str::FromStr};

use bitcoin::{
    hash_types::Txid, OutPoint, ScriptBuf, Sequence, Transaction, TxIn as BitcoinTxIn, Witness,
};
use config::Config;
use dogecoin::{
    try_debug, try_warn,
    types::{
        dogecoin::TxOut, BlockIdentifier, DogecoinBlockData, DogecoinNetwork,
        DogecoinTransactionData, OrdinalInscriptionCurseType, OrdinalInscriptionNumber,
        OrdinalInscriptionRevealData, OrdinalOperation,
    },
    utils::Context,
};
use doginals::{
    envelope::{Envelope, ParsedEnvelope},
    inscription::Inscription,
    inscription_id::InscriptionId,
};
use serde_json::json;

use crate::core::meta_protocols::dmp::{try_parse_dmp, DmpOperation};
use crate::core::meta_protocols::dns::try_parse_dns_name;
use crate::core::meta_protocols::dogemap::try_parse_dogemap_claim;
use crate::core::meta_protocols::drc20::{
    drc20_activation_height,
    parser::{parse_drc20_operation, ParsedDrc20Operation},
};
use crate::core::meta_protocols::lotto::{
    try_parse_lotto_deploy, try_parse_lotto_mint, LottoDeploy, LottoMint,
};

/// A DMP operation parsed from an inscription body, bundled with context.
#[derive(Debug, Clone)]
pub struct ParsedDmpOp {
    pub inscription_id: String,
    pub tx_id: String,
    pub op: DmpOperation,
    pub block_height: u64,
    pub block_timestamp: u32,
}

#[derive(Debug, Clone)]
pub struct ParsedLottoDeploy {
    pub inscription_id: String,
    pub tx_id: String,
    pub deploy: LottoDeploy,
}

#[derive(Debug, Clone)]
pub struct ParsedLottoOutput {
    pub value: u64,
    pub script_pubkey: String,
}

#[derive(Debug, Clone)]
pub struct ParsedLottoMint {
    pub inscription_id: String,
    pub tx_id: String,
    pub outputs: Vec<ParsedLottoOutput>,
    pub mint: LottoMint,
}

/// Bitcoin/Taproot only — Dogecoin has no witness data.
/// This path is never reached during Dogecoin indexing; `parse_inscriptions_from_standardized_tx`
/// handles all Dogecoin inscription parsing via script_sig + `from_transactions_dogecoin`.
#[allow(dead_code, deprecated)]
pub fn parse_inscriptions_from_witness(
    input_index: usize,
    witness_bytes: Vec<Vec<u8>>,
    txid: &str,
) -> Option<Vec<(OrdinalInscriptionRevealData, Inscription)>> {
    let witness = Witness::from_slice(&witness_bytes);
    let tapscript = witness.tapscript()?;
    let envelopes: Vec<Envelope<Inscription>> = Envelope::from_tapscript(tapscript, input_index)
        .ok()?
        .into_iter()
        .map(ParsedEnvelope::from)
        .collect();
    let mut inscriptions = vec![];
    for envelope in envelopes.into_iter() {
        let curse_type = if envelope.payload.unrecognized_even_field {
            Some(OrdinalInscriptionCurseType::UnrecognizedEvenField)
        } else if envelope.payload.duplicate_field {
            Some(OrdinalInscriptionCurseType::DuplicateField)
        } else if envelope.payload.incomplete_field {
            Some(OrdinalInscriptionCurseType::IncompleteField)
        } else if envelope.input != 0 {
            Some(OrdinalInscriptionCurseType::NotInFirstInput)
        } else if envelope.offset != 0 {
            Some(OrdinalInscriptionCurseType::NotAtOffsetZero)
        } else if envelope.payload.pointer.is_some() {
            Some(OrdinalInscriptionCurseType::Pointer)
        } else if envelope.pushnum {
            Some(OrdinalInscriptionCurseType::Pushnum)
        } else if envelope.stutter {
            Some(OrdinalInscriptionCurseType::Stutter)
        } else {
            None
        };

        let inscription_id = InscriptionId {
            txid: Txid::from_str(txid).unwrap(),
            index: input_index as u32,
        };

        let no_content_bytes = vec![];
        let inscription_content_bytes = envelope.payload.body().unwrap_or(&no_content_bytes);
        let mut content_bytes = "0x".to_string();
        content_bytes.push_str(&hex::encode(inscription_content_bytes));

        let parents = envelope
            .payload
            .parents()
            .iter()
            .map(|i| i.to_string())
            .collect();
        let delegate = envelope.payload.delegate().map(|i| i.to_string());
        let metaprotocol = envelope.payload.metaprotocol().map(|p| p.to_string());
        let metadata = envelope.payload.metadata().map(|m| json!(m));

        // Most of these fields will be calculated later when we know for certain which satoshi contains this inscription.
        let reveal_data = OrdinalInscriptionRevealData {
            content_type: envelope.payload.content_type().unwrap_or("").to_string(),
            content_bytes,
            content_length: inscription_content_bytes.len(),
            inscription_id: inscription_id.to_string(),
            inscription_input_index: input_index,
            tx_index: 0,
            inscription_output_value: 0,
            inscription_pointer: envelope.payload.pointer(),
            inscription_fee: 0,
            inscription_number: OrdinalInscriptionNumber::zero(),
            inscriber_address: None,
            parents,
            delegate,
            metaprotocol,
            metadata,
            ordinal_number: 0,
            ordinal_block_height: 0,
            ordinal_offset: 0,
            transfers_pre_inscription: 0,
            koinupoint_post_inscription: String::new(),
            curse_type,
            dogespells: 0,
            unbound_sequence: None,
        };
        inscriptions.push((reveal_data, envelope.payload));
    }
    Some(inscriptions)
}

fn quick_mime_or_prefix_match(
    content_type: &str,
    body: Option<&Vec<u8>>,
    prefixes: &[String],
) -> bool {
    if prefixes.is_empty() {
        return true;
    }
    if prefixes.iter().any(|p| content_type.starts_with(p)) {
        return true;
    }
    if let Some(b) = body {
        for p in prefixes {
            if b.starts_with(p.as_bytes()) {
                return true;
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
pub fn parse_inscriptions_from_standardized_tx(
    tx: &mut DogecoinTransactionData,
    block_identifier: &BlockIdentifier,
    network: &DogecoinNetwork,
    drc20_operation_map: &mut HashMap<String, ParsedDrc20Operation>,
    dns_map: &mut HashMap<String, String>,
    dogemap_map: &mut HashMap<u32, String>,
    lotto_deploy_map: &mut HashMap<String, ParsedLottoDeploy>,
    lotto_mints: &mut Vec<ParsedLottoMint>,
    dmp_ops: &mut Vec<ParsedDmpOp>,
    config: &Config,
    ctx: &Context,
) -> Vec<OrdinalOperation> {
    let mut operations = vec![];

    // Dogecoin uses script_sig for inscriptions, not witness data.
    // We need to convert the Dogecoin transaction data into a Bitcoin Transaction
    // so we can use the envelope parsing logic.

    if tx.metadata.inputs.is_empty() {
        return operations;
    }

    // Build a Bitcoin Transaction from Dogecoin transaction data
    let bitcoin_tx = Transaction {
        version: bitcoin::transaction::Version(2),
        lock_time: bitcoin::blockdata::locktime::absolute::LockTime::ZERO,
        input: tx
            .metadata
            .inputs
            .iter()
            .map(|input| {
                // Decode the script_sig from hex string
                let script_bytes = if input.script_sig.starts_with("0x") {
                    hex::decode(&input.script_sig[2..]).unwrap_or_default()
                } else {
                    hex::decode(&input.script_sig).unwrap_or_default()
                };

                BitcoinTxIn {
                    previous_output: OutPoint::null(),
                    script_sig: ScriptBuf::from_bytes(script_bytes),
                    sequence: Sequence::from_consensus(input.sequence),
                    witness: Witness::new(), // Dogecoin doesn't use witness
                }
            })
            .collect(),
        output: Vec::new(),
    };

    // Parse inscriptions from script_sig using Dogecoin parsing method
    let envelopes = ParsedEnvelope::from_transactions_dogecoin(&[bitcoin_tx]);

    for envelope in envelopes.into_iter() {
        let input_index = envelope.input as usize;
        let inscription = envelope.payload;

        let curse_type = if inscription.unrecognized_even_field {
            Some(OrdinalInscriptionCurseType::UnrecognizedEvenField)
        } else if inscription.duplicate_field {
            Some(OrdinalInscriptionCurseType::DuplicateField)
        } else if inscription.incomplete_field {
            Some(OrdinalInscriptionCurseType::IncompleteField)
        } else if envelope.input != 0 {
            Some(OrdinalInscriptionCurseType::NotInFirstInput)
        } else if envelope.offset != 0 {
            Some(OrdinalInscriptionCurseType::NotAtOffsetZero)
        } else if inscription.pointer.is_some() {
            Some(OrdinalInscriptionCurseType::Pointer)
        } else if envelope.pushnum {
            Some(OrdinalInscriptionCurseType::Pushnum)
        } else if envelope.stutter {
            Some(OrdinalInscriptionCurseType::Stutter)
        } else {
            None
        };

        let inscription_id = InscriptionId {
            txid: Txid::from_str(tx.transaction_identifier.get_hash_bytes_str()).unwrap(),
            index: input_index as u32,
        };

        let no_content_bytes = vec![];
        let inscription_content_bytes = inscription.body.as_ref().unwrap_or(&no_content_bytes);
        let mut content_bytes = "0x".to_string();
        content_bytes.push_str(&hex::encode(inscription_content_bytes));

        let parents = inscription
            .parents
            .iter()
            .map(|bytes| {
                let id = InscriptionId::from_value(bytes).ok();
                id.map(|i| i.to_string()).unwrap_or_default()
            })
            .collect();

        let delegate = inscription
            .delegate
            .as_ref()
            .and_then(|bytes| InscriptionId::from_value(bytes).ok().map(|i| i.to_string()));

        let metaprotocol = inscription
            .metaprotocol
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).to_string());

        let metadata = inscription.metadata.as_ref().and_then(|bytes| {
            ciborium::de::from_reader::<ciborium::Value, _>(bytes.as_slice())
                .ok()
                .map(|v| json!(v))
        });

        let content_type = inscription
            .content_type
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).to_string())
            .unwrap_or_default();

        // Most of these fields will be calculated later when we know for certain which koinu contains this inscription.
        let reveal_data = OrdinalInscriptionRevealData {
            content_type,
            content_bytes,
            content_length: inscription_content_bytes.len(),
            inscription_id: inscription_id.to_string(),
            inscription_input_index: input_index,
            tx_index: 0,
            inscription_output_value: 0,
            inscription_pointer: inscription.pointer.as_ref().and_then(|bytes| {
                if bytes.len() <= 8 {
                    let mut array = [0u8; 8];
                    array[..bytes.len()].copy_from_slice(bytes);
                    Some(u64::from_le_bytes(array))
                } else {
                    None
                }
            }),
            inscription_fee: 0,
            inscription_number: OrdinalInscriptionNumber::zero(),
            inscriber_address: None,
            parents,
            delegate,
            metaprotocol,
            metadata,
            ordinal_number: 0,
            ordinal_block_height: 0,
            ordinal_offset: 0,
            transfers_pre_inscription: 0,
            koinupoint_post_inscription: String::new(),
            curse_type,
            dogespells: 0,
            unbound_sequence: None,
        };

        // Check for DRC-20 operations
        if let Some(drc20) = config.ordinals_drc20_config() {
            if drc20.enabled
                && block_identifier.index >= drc20_activation_height(network)
                && (content_type.starts_with("application/json")
                    || inscription_content_bytes.starts_with(b"{"))
            {
                match parse_drc20_operation(&inscription) {
                    Ok(Some(op)) => {
                        drc20_operation_map.insert(reveal_data.inscription_id.clone(), op);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        try_warn!(ctx, "Error parsing DRC-20 operation: {}", e);
                    }
                };
            }
        }

        // DNS detection — runs before the predicate filter so DNS names are
        // never accidentally excluded by content-type predicates.
        if config.dns_enabled() {
            if let Some(body) = inscription.body.as_ref() {
                if let Some(name) = try_parse_dns_name(body) {
                    dns_map
                        .entry(name)
                        .or_insert_with(|| reveal_data.inscription_id.clone());
                }
            }
        }

        // Dogemap detection — same reasoning as DNS above.
        if config.dogemap_enabled() {
            if let Some(body) = inscription.body.as_ref() {
                if let Some(block_number) = try_parse_dogemap_claim(body, block_identifier.index) {
                    dogemap_map
                        .entry(block_number)
                        .or_insert_with(|| reveal_data.inscription_id.clone());
                }
            }
        }

        // DogeLotto detection mirrors DNS/Dogemap: parse before global predicates so
        // protocol activity is never dropped by selective indexing rules.
        if config.lotto_enabled()
            && quick_mime_or_prefix_match(
                &content_type,
                inscription.body.as_ref(),
                &config.protocols.lotto.content_prefixes,
            )
            && crate::core::protocol::predicate::inscription_matches_content_prefixes(
                &inscription,
                &config.protocols.lotto.content_prefixes,
            )
        {
            if let Some(body) = inscription.body.as_ref() {
                if let Some(deploy) = try_parse_lotto_deploy(body) {
                    lotto_deploy_map
                        .entry(deploy.lotto_id.clone())
                        .or_insert_with(|| ParsedLottoDeploy {
                            inscription_id: reveal_data.inscription_id.clone(),
                            tx_id: tx.transaction_identifier.get_hash_bytes_str().to_string(),
                            deploy,
                        });
                } else if let Some(mint) = try_parse_lotto_mint(body) {
                    lotto_mints.push(ParsedLottoMint {
                        inscription_id: reveal_data.inscription_id.clone(),
                        tx_id: tx.transaction_identifier.get_hash_bytes_str().to_string(),
                        // Persist all tx outputs so the DB layer can verify the exact
                        // prize-pool payment occurred in this same inscription tx.
                        outputs: tx
                            .metadata
                            .outputs
                            .iter()
                            .cloned()
                            .map(parsed_lotto_output_from_txout)
                            .collect(),
                        mint,
                    });
                }
            }
        }

        // DMP detection — parse before predicate filter so market activity is never dropped.
        if config.dmp_enabled() {
            if let Some(body) = inscription.body.as_ref() {
                if let Some(op) = try_parse_dmp(body, &reveal_data.inscription_id) {
                    dmp_ops.push(ParsedDmpOp {
                        inscription_id: reveal_data.inscription_id.clone(),
                        tx_id: tx.transaction_identifier.get_hash_bytes_str().to_string(),
                        op,
                        block_height: block_identifier.index,
                        block_timestamp: 0, // filled in by the block-level caller
                    });
                }
            }
        }

        // Hiro-style predicate filtering: skip inscriptions that don't match the configured rules.
        if let Some(predicates) = config.doginals_predicates() {
            if !crate::core::protocol::predicate::inscription_matches_predicates(
                &inscription,
                predicates,
            ) {
                try_debug!(
                    ctx,
                    "Inscription {} filtered by predicate",
                    reveal_data.inscription_id
                );
                continue;
            }
        }

        operations.push(OrdinalOperation::InscriptionRevealed(reveal_data));
    }

    operations
}

fn parsed_lotto_output_from_txout(output: TxOut) -> ParsedLottoOutput {
    ParsedLottoOutput {
        value: output.value,
        script_pubkey: output.script_pubkey,
    }
}

pub fn parse_inscriptions_in_standardized_block(
    block: &mut DogecoinBlockData,
    drc20_operation_map: &mut HashMap<String, ParsedDrc20Operation>,
    dns_map: &mut HashMap<String, String>,
    dogemap_map: &mut HashMap<u32, String>,
    lotto_deploy_map: &mut HashMap<String, ParsedLottoDeploy>,
    lotto_mints: &mut Vec<ParsedLottoMint>,
    dmp_ops: &mut Vec<ParsedDmpOp>,
    config: &Config,
    ctx: &Context,
) {
    let block_timestamp = block.timestamp;
    for tx in block.transactions.iter_mut() {
        let start_idx = dmp_ops.len();
        tx.metadata.ordinal_operations = parse_inscriptions_from_standardized_tx(
            tx,
            &block.block_identifier,
            &block.metadata.network,
            drc20_operation_map,
            dns_map,
            dogemap_map,
            lotto_deploy_map,
            lotto_mints,
            dmp_ops,
            config,
            ctx,
        );
        // Back-fill block_timestamp for DMP ops emitted by this tx
        for op in dmp_ops[start_idx..].iter_mut() {
            op.block_timestamp = block_timestamp;
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use config::Config;
    use dogecoin::{types::OrdinalOperation, utils::Context};

    use super::parse_inscriptions_in_standardized_block;
    use crate::core::test_builders::{TestBlockBuilder, TestTransactionBuilder, TestTxInBuilder};

    #[test]
    fn parses_inscriptions_in_block() {
        let ctx = Context::empty();
        let config = Config::test_default();
        let mut block = TestBlockBuilder::new()
            .add_transaction(
                TestTransactionBuilder::new()
                    .add_input(
                        TestTxInBuilder::new()
                            .witness(vec![
                                "0x6c00eb3c4d35fedd257051333b4ca81d1a25a37a9af4891f1fec2869edd56b14180eafbda8851d63138a724c9b15384bc5f0536de658bd294d426a36212e6f08".to_string(),
                                "0x209e2849b90a2353691fccedd467215c88eec89a5d0dcf468e6cf37abed344d746ac0063036f7264010118746578742f706c61696e3b636861727365743d7574662d38004c5e7b200a20202270223a20226272632d3230222c0a2020226f70223a20226465706c6f79222c0a2020227469636b223a20226f726469222c0a2020226d6178223a20223231303030303030222c0a2020226c696d223a202231303030220a7d68".to_string(),
                                "0xc19e2849b90a2353691fccedd467215c88eec89a5d0dcf468e6cf37abed344d746".to_string(),
                            ])
                            .build()
                    )
                    .build(),
            )
            .build();
        parse_inscriptions_in_standardized_block(
            &mut block,
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            &config,
            &ctx,
        );
        let OrdinalOperation::InscriptionRevealed(reveal) =
            &block.transactions[0].metadata.ordinal_operations[0]
        else {
            panic!();
        };
        assert_eq!(
            reveal.inscription_id,
            "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735i0".to_string()
        );
        assert_eq!(reveal.content_bytes, "0x7b200a20202270223a20226272632d3230222c0a2020226f70223a20226465706c6f79222c0a2020227469636b223a20226f726469222c0a2020226d6178223a20223231303030303030222c0a2020226c696d223a202231303030220a7d".to_string());
        assert_eq!(reveal.content_length, 94);
    }
}
