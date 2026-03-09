use std::collections::{BTreeMap, HashMap};

use bitcoin::ScriptBuf;
use deadpool_postgres::GenericClient;
use dogecoin::types::{
    BlockIdentifier, DogecoinBlockData, OrdinalInscriptionNumber, OrdinalOperation,
    TransactionIdentifier,
};
use postgres::{
    types::{PgBigIntU32, PgNumericU64},
    utils,
};
use refinery::embed_migrations;
use sha2::{Digest, Sha256};
use tokio_postgres::{types::ToSql, Client};

use super::models::{
    DbCurrentLocation, DbInscription, DbInscriptionParent, DbInscriptionRecursion, DbKoinu,
    DbLocation,
};
use crate::core::meta_protocols::lotto::{
    derive_draw_for_deploy, score_ticket, validate_mint_against_deploy, LottoDeploy, LottoDraw,
    LottoTemplate, NumberConfig, ResolutionMode,
};
use crate::core::protocol::{
    inscription_parsing::{ParsedLottoDeploy, ParsedLottoMint},
    koinu_numbering::TraversalResult,
    koinu_tracking::WatchedSatpoint,
};

embed_migrations!("../../migrations/doginals");
pub async fn migrate(client: &mut Client) -> Result<(), String> {
    return match migrations::runner()
        .set_abort_divergent(false)
        .set_abort_missing(false)
        .set_migration_table_name("pgmigrations")
        .run_async(client)
        .await
    {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("Error running pg migrations: {e}")),
    };
}

pub async fn get_chain_tip<T: GenericClient>(
    client: &T,
) -> Result<Option<BlockIdentifier>, String> {
    let row = client
        .query_opt("SELECT block_height, block_hash FROM chain_tip", &[])
        .await
        .map_err(|e| format!("get_chain_tip: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let height: Option<PgNumericU64> = row.get("block_height");
    let hash: Option<String> = row.get("block_hash");
    if let (Some(height), Some(hash)) = (height, hash) {
        Ok(Some(BlockIdentifier {
            index: height.0,
            hash: format!("0x{hash}"),
        }))
    } else {
        Ok(None)
    }
}

pub async fn get_chain_tip_block_height<T: GenericClient>(
    client: &T,
) -> Result<Option<u64>, String> {
    let row = client
        .query_opt("SELECT block_height FROM chain_tip", &[])
        .await
        .map_err(|e| format!("get_chain_tip_block_height: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let max: Option<PgNumericU64> = row.get("block_height");
    Ok(max.map(|v| v.0))
}

pub async fn get_highest_inscription_number<T: GenericClient>(
    client: &T,
) -> Result<Option<i64>, String> {
    let row = client
        .query_opt("SELECT MAX(number) AS max FROM inscriptions", &[])
        .await
        .map_err(|e| format!("get_highest_inscription_number: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let max: Option<i64> = row.get("max");
    Ok(max)
}

pub async fn get_highest_blessed_classic_inscription_number<T: GenericClient>(
    client: &T,
) -> Result<Option<i64>, String> {
    let row = client
        .query_opt(
            "SELECT MAX(classic_number) AS max FROM inscriptions WHERE classic_number >= 0",
            &[],
        )
        .await
        .map_err(|e| format!("get_highest_blessed_classic_inscription_number: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let max: Option<i64> = row.get("max");
    Ok(max)
}

pub async fn get_lowest_cursed_classic_inscription_number<T: GenericClient>(
    client: &T,
) -> Result<Option<i64>, String> {
    let row = client
        .query_opt(
            "SELECT MIN(classic_number) AS min FROM inscriptions WHERE classic_number < 0",
            &[],
        )
        .await
        .map_err(|e| format!("get_lowest_cursed_classic_inscription_number: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let min: Option<i64> = row.get("min");
    Ok(min)
}

pub async fn get_blessed_count_from_counts_by_type<T: GenericClient>(
    client: &T,
) -> Result<Option<i64>, String> {
    let row = client
        .query_opt(
            "SELECT count FROM counts_by_type WHERE type = 'blessed'",
            &[],
        )
        .await
        .map_err(|e| format!("get_blessed_count_from_counts_by_type: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let count: i32 = row.get("count");
    let big_count: i64 = count.into();
    Ok(Some(big_count))
}

pub async fn get_cursed_count_from_counts_by_type<T: GenericClient>(
    client: &T,
) -> Result<Option<i64>, String> {
    let row = client
        .query_opt(
            "SELECT count FROM counts_by_type WHERE type = 'cursed'",
            &[],
        )
        .await
        .map_err(|e| format!("get_cursed_count_from_counts_by_type: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let count: i32 = row.get("count");
    let big_count: i64 = count.into();
    Ok(Some(big_count))
}

pub async fn get_highest_unbound_inscription_sequence<T: GenericClient>(
    client: &T,
) -> Result<Option<i64>, String> {
    let row = client
        .query_opt("SELECT MAX(unbound_sequence) AS max FROM inscriptions", &[])
        .await
        .map_err(|e| format!("get_highest_unbound_inscription_sequence: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let max: Option<i64> = row.get("max");
    Ok(max)
}

pub async fn get_reinscriptions_for_block<T: GenericClient>(
    inscriptions_data: &mut BTreeMap<(TransactionIdentifier, usize, u64), TraversalResult>,
    client: &T,
) -> Result<HashMap<u64, String>, String> {
    let mut ordinal_numbers = vec![];
    for value in inscriptions_data.values() {
        if value.ordinal_number != 0 {
            ordinal_numbers.push(PgNumericU64(value.ordinal_number));
        }
    }
    let number_refs: Vec<&PgNumericU64> = ordinal_numbers.iter().collect();
    let rows = client
        .query(
            "SELECT ordinal_number, inscription_id
            FROM inscriptions
            WHERE ordinal_number = ANY ($1) AND classic_number >= 0",
            &[&number_refs],
        )
        .await
        .map_err(|e| format!("get_reinscriptions_for_block: {e}"))?;
    let mut results = HashMap::new();
    for row in rows.iter() {
        let ordinal_number: PgNumericU64 = row.get("ordinal_number");
        let inscription_id: String = row.get("inscription_id");
        results.insert(ordinal_number.0, inscription_id);
    }
    Ok(results)
}

pub async fn has_ordinal_activity_at_block<T: GenericClient>(
    client: &T,
    block_height: u64,
) -> Result<bool, String> {
    let row = client
        .query_opt(
            "SELECT 1 FROM locations WHERE block_height = $1 LIMIT 1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .map_err(|e| format!("has_ordinal_activity_at_block: {e}"))?;
    Ok(row.is_some())
}

pub async fn get_inscriptions_at_block<T: GenericClient>(
    client: &T,
    block_height: u64,
) -> Result<BTreeMap<String, TraversalResult>, String> {
    let rows = client
        .query(
            "SELECT number, classic_number, ordinal_number, inscription_id, input_index, tx_id
            FROM inscriptions
            WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .map_err(|e| format!("get_inscriptions_at_block: {e}"))?;
    let mut results = BTreeMap::new();
    for row in rows.iter() {
        let inscription_number = OrdinalInscriptionNumber {
            classic: row.get("classic_number"),
            jubilee: row.get("number"),
        };
        let ordinal_number: PgNumericU64 = row.get("ordinal_number");
        let inscription_id: String = row.get("inscription_id");
        let inscription_input_index: PgBigIntU32 = row.get("input_index");
        let tx_id: String = row.get("tx_id");
        let traversal = TraversalResult {
            inscription_number,
            ordinal_number: ordinal_number.0,
            inscription_input_index: inscription_input_index.0 as usize,
            transfers: 0,
            transaction_identifier_inscription: TransactionIdentifier { hash: tx_id },
        };
        results.insert(inscription_id, traversal);
    }
    Ok(results)
}

pub async fn get_inscribed_satpoints_at_tx_inputs<T: GenericClient>(
    inputs: &[(usize, String)],
    client: &T,
) -> Result<HashMap<usize, Vec<WatchedSatpoint>>, String> {
    let mut results = HashMap::new();
    if inputs.is_empty() {
        return Ok(results);
    }
    for chunk in inputs.chunks(500) {
        let outpoints: Vec<(String, String)> = chunk
            .iter()
            .map(|(vin, satpoint)| (vin.to_string(), satpoint.clone()))
            .collect();
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for (vin, input) in outpoints.iter() {
            params.push(vin);
            params.push(input);
        }
        let rows = client
            .query(
                &format!(
                    "WITH inputs (vin, output) AS (VALUES {})
                    SELECT i.vin, l.ordinal_number, l.\"offset\"
                    FROM current_locations AS l
                    INNER JOIN inputs AS i ON i.output = l.output",
                    utils::multi_row_query_param_str(chunk.len(), 2)
                ),
                &params,
            )
            .await
            .map_err(|e| format!("get_inscriptions_at_tx_inputs: {e}"))?;
        for row in rows.iter() {
            let vin: String = row.get("vin");
            let vin_key = vin.parse::<usize>().unwrap();
            let ordinal_number: PgNumericU64 = row.get("ordinal_number");
            let offset: PgNumericU64 = row.get("offset");
            let entry = results.entry(vin_key).or_insert(vec![]);
            entry.push(WatchedSatpoint {
                ordinal_number: ordinal_number.0,
                offset: offset.0,
            });
        }
    }
    Ok(results)
}

async fn insert_inscriptions<T: GenericClient>(
    inscriptions: &[DbInscription],
    client: &T,
) -> Result<(), String> {
    if inscriptions.is_empty() {
        return Ok(());
    }
    for chunk in inscriptions.chunks(500) {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            params.push(&row.inscription_id);
            params.push(&row.ordinal_number);
            params.push(&row.number);
            params.push(&row.classic_number);
            params.push(&row.block_height);
            params.push(&row.block_hash);
            params.push(&row.tx_id);
            params.push(&row.tx_index);
            params.push(&row.address);
            params.push(&row.mime_type);
            params.push(&row.content_type);
            params.push(&row.content_length);
            params.push(&row.content);
            params.push(&row.fee);
            params.push(&row.curse_type);
            params.push(&row.recursive);
            params.push(&row.input_index);
            params.push(&row.pointer);
            params.push(&row.metadata);
            params.push(&row.metaprotocol);
            params.push(&row.delegate);
            params.push(&row.timestamp);
            params.push(&row.charms);
            params.push(&row.unbound_sequence);
        }
        client
            .query(
                &format!("INSERT INTO inscriptions
                    (inscription_id, ordinal_number, number, classic_number, block_height, block_hash, tx_id, tx_index, address,
                    mime_type, content_type, content_length, content, fee, curse_type, recursive, input_index, pointer, metadata,
                    metaprotocol, delegate, timestamp, charms, unbound_sequence)
                    VALUES {}
                    ON CONFLICT (number) DO NOTHING", utils::multi_row_query_param_str(chunk.len(), 24)),
                &params,
            )
            .await
            .map_err(|e| format!("insert_inscriptions: {e}"))?;
    }
    Ok(())
}

async fn insert_inscription_recursions<T: GenericClient>(
    inscription_recursions: &[DbInscriptionRecursion],
    client: &T,
) -> Result<(), String> {
    if inscription_recursions.is_empty() {
        return Ok(());
    }
    for chunk in inscription_recursions.chunks(500) {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            params.push(&row.inscription_id);
            params.push(&row.ref_inscription_id);
        }
        client
            .query(
                &format!(
                    "INSERT INTO inscription_recursions
                    (inscription_id, ref_inscription_id)
                    VALUES {}
                    ON CONFLICT (inscription_id, ref_inscription_id) DO NOTHING",
                    utils::multi_row_query_param_str(chunk.len(), 2)
                ),
                &params,
            )
            .await
            .map_err(|e| format!("insert_inscription_recursions: {e}"))?;
    }
    Ok(())
}

async fn insert_inscription_parents<T: GenericClient>(
    inscription_parents: &[DbInscriptionParent],
    client: &T,
) -> Result<(), String> {
    if inscription_parents.is_empty() {
        return Ok(());
    }
    for chunk in inscription_parents.chunks(500) {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            params.push(&row.inscription_id);
            params.push(&row.parent_inscription_id);
        }
        client
            .query(
                &format!(
                    "INSERT INTO inscription_parents
                    (inscription_id, parent_inscription_id)
                    VALUES {}
                    ON CONFLICT (inscription_id, parent_inscription_id) DO NOTHING",
                    utils::multi_row_query_param_str(chunk.len(), 2)
                ),
                &params,
            )
            .await
            .map_err(|e| format!("insert_inscription_parents: {e}"))?;
    }
    Ok(())
}

async fn insert_locations<T: GenericClient>(
    locations: &[DbLocation],
    client: &T,
) -> Result<(), String> {
    if locations.is_empty() {
        return Ok(());
    }
    for chunk in locations.chunks(500) {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            params.push(&row.ordinal_number);
            params.push(&row.block_height);
            params.push(&row.tx_index);
            params.push(&row.tx_id);
            params.push(&row.block_hash);
            params.push(&row.address);
            params.push(&row.output);
            params.push(&row.offset);
            params.push(&row.prev_output);
            params.push(&row.prev_offset);
            params.push(&row.value);
            params.push(&row.transfer_type);
            params.push(&row.timestamp);
        }
        // Insert locations but also calculate inscription transfers, keeping in mind transfers could come from within an earlier
        // tx in the same block.
        client
            .query(
                &format!(
                    "WITH location_inserts AS (
                        INSERT INTO locations (ordinal_number, block_height, tx_index, tx_id, block_hash, address, output,
                            \"offset\", prev_output, prev_offset, value, transfer_type, timestamp)
                        VALUES {}
                        ON CONFLICT (ordinal_number, block_height, tx_index) DO NOTHING
                        RETURNING ordinal_number, block_height, block_hash, tx_index
                    ),
                    prev_transfer_index AS (
                        SELECT MAX(block_transfer_index) AS max
                        FROM inscription_transfers
                        WHERE block_height = (SELECT block_height FROM location_inserts LIMIT 1)
                    ),
                    moved_inscriptions AS (
                        SELECT i.inscription_id, i.number, i.ordinal_number, li.block_height, li.tx_index,
                            COALESCE(
                                (
                                    SELECT l.block_height || ',' || l.tx_index
                                    FROM locations AS l
                                    WHERE l.ordinal_number = li.ordinal_number AND (
                                        l.block_height < li.block_height OR
                                        (l.block_height = li.block_height AND l.tx_index < li.tx_index)
                                    )
                                    ORDER BY l.block_height DESC, l.tx_index DESC
                                    LIMIT 1
                                ),
                                (
                                    SELECT l.block_height || ',' || l.tx_index
                                    FROM location_inserts AS l
                                    WHERE l.ordinal_number = li.ordinal_number AND (
                                        l.block_height < li.block_height OR
                                        (l.block_height = li.block_height AND l.tx_index < li.tx_index)
                                    )
                                    ORDER BY l.block_height DESC, l.tx_index DESC
                                    LIMIT 1
                                )
                            ) AS from_data,
                            (ROW_NUMBER() OVER (ORDER BY li.block_height ASC, li.tx_index ASC) + (SELECT COALESCE(max, -1) FROM prev_transfer_index)) AS block_transfer_index
                        FROM inscriptions AS i
                        INNER JOIN location_inserts AS li ON li.ordinal_number = i.ordinal_number
                        WHERE i.block_height < li.block_height OR (i.block_height = li.block_height AND i.tx_index < li.tx_index)
                    )
                    INSERT INTO inscription_transfers
                        (inscription_id, number, ordinal_number, block_height, tx_index, from_block_height, from_tx_index, block_transfer_index)
                        (
                            SELECT inscription_id, number, ordinal_number, block_height, tx_index,
                                SPLIT_PART(from_data, ',', 1)::numeric AS from_block_height,
                                SPLIT_PART(from_data, ',', 2)::bigint AS from_tx_index,
                                block_transfer_index
                            FROM moved_inscriptions
                        )
                        ON CONFLICT (block_height, block_transfer_index) DO NOTHING",
                    utils::multi_row_query_param_str(chunk.len(), 13)
                ),
                &params,
            )
            .await
            .map_err(|e| format!("insert_locations: {e}"))?;
    }
    Ok(())
}

async fn insert_koinus<T: GenericClient>(satoshis: &[DbKoinu], client: &T) -> Result<(), String> {
    if satoshis.is_empty() {
        return Ok(());
    }
    for chunk in satoshis.chunks(500) {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            params.push(&row.ordinal_number);
            params.push(&row.rarity);
            params.push(&row.coinbase_height);
        }
        client
            .query(
                &format!(
                    "INSERT INTO satoshis
                    (ordinal_number, rarity, coinbase_height)
                    VALUES {}
                    ON CONFLICT (ordinal_number) DO NOTHING",
                    utils::multi_row_query_param_str(chunk.len(), 3)
                ),
                &params,
            )
            .await
            .map_err(|e| format!("insert_koinus: {e}"))?;
    }
    Ok(())
}

async fn insert_current_locations<T: GenericClient>(
    current_locations: &HashMap<PgNumericU64, DbCurrentLocation>,
    client: &T,
) -> Result<(), String> {
    let moved_sats: Vec<&PgNumericU64> = current_locations.keys().collect();
    let new_locations: Vec<&DbCurrentLocation> = current_locations.values().collect();
    // Deduct counts from previous owners
    for chunk in moved_sats.chunks(500) {
        let c = chunk.to_vec();
        client
            .query(
                "WITH prev_owners AS (
                    SELECT address, COUNT(*) AS count
                    FROM current_locations
                    WHERE ordinal_number = ANY ($1)
                    GROUP BY address
                )
                UPDATE counts_by_address
                SET count = (
                    SELECT counts_by_address.count - p.count
                    FROM prev_owners AS p
                    WHERE p.address = counts_by_address.address
                )
                WHERE EXISTS (SELECT 1 FROM prev_owners AS p WHERE p.address = counts_by_address.address)",
                &[&c],
            )
            .await
            .map_err(|e| format!("insert_current_locations: {e}"))?;
    }
    // Insert locations
    for chunk in new_locations.chunks(500) {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            params.push(&row.ordinal_number);
            params.push(&row.block_height);
            params.push(&row.tx_id);
            params.push(&row.tx_index);
            params.push(&row.address);
            params.push(&row.output);
            params.push(&row.offset);
        }
        client
            .query(
                &format!(
                    "INSERT INTO current_locations (ordinal_number, block_height, tx_id, tx_index, address, output, \"offset\")
                    VALUES {}
                    ON CONFLICT (ordinal_number) DO UPDATE SET
                        block_height = EXCLUDED.block_height,
                        tx_id = EXCLUDED.tx_id,
                        tx_index = EXCLUDED.tx_index,
                        address = EXCLUDED.address,
                        output = EXCLUDED.output,
                        \"offset\" = EXCLUDED.\"offset\"
                    WHERE
                        EXCLUDED.block_height > current_locations.block_height OR
                        (EXCLUDED.block_height = current_locations.block_height AND
                            EXCLUDED.tx_index > current_locations.tx_index)",
                    utils::multi_row_query_param_str(chunk.len(), 7)
                ),
                &params,
            )
            .await
            .map_err(|e| format!("insert_current_locations: {e}"))?;
    }
    // Update owner counts
    for chunk in moved_sats.chunks(500) {
        let c = chunk.to_vec();
        client
            .query(
                "WITH new_owners AS (
                    SELECT address, COUNT(*) AS count
                    FROM current_locations
                    WHERE ordinal_number = ANY ($1) AND address IS NOT NULL
                    GROUP BY address
                )
                INSERT INTO counts_by_address (address, count)
                (SELECT address, count FROM new_owners)
                ON CONFLICT (address) DO UPDATE SET count = counts_by_address.count + EXCLUDED.count",
                &[&c],
            )
            .await
            .map_err(|e| format!("insert_current_locations: {e}"))?;
    }
    Ok(())
}

async fn update_mime_type_counts<T: GenericClient>(
    counts: &HashMap<String, i32>,
    client: &T,
) -> Result<(), String> {
    if counts.is_empty() {
        return Ok(());
    }
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
    for (key, value) in counts {
        params.push(key);
        params.push(value);
    }
    client
        .query(
            &format!(
                "INSERT INTO counts_by_mime_type (mime_type, count) VALUES {}
                ON CONFLICT (mime_type) DO UPDATE SET count = counts_by_mime_type.count + EXCLUDED.count",
                utils::multi_row_query_param_str(counts.len(), 2)
            ),
            &params,
        )
        .await
        .map_err(|e| format!("update_mime_type_counts: {e}"))?;
    Ok(())
}

async fn update_sat_rarity_counts<T: GenericClient>(
    counts: &HashMap<String, i32>,
    client: &T,
) -> Result<(), String> {
    if counts.is_empty() {
        return Ok(());
    }
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
    for (key, value) in counts {
        params.push(key);
        params.push(value);
    }
    client
        .query(
            &format!(
                "INSERT INTO counts_by_sat_rarity (rarity, count) VALUES {}
                ON CONFLICT (rarity) DO UPDATE SET count = counts_by_sat_rarity.count + EXCLUDED.count",
                utils::multi_row_query_param_str(counts.len(), 2)
            ),
            &params,
        )
        .await
        .map_err(|e| format!("update_sat_rarity_counts: {e}"))?;
    Ok(())
}

async fn update_inscription_type_counts<T: GenericClient>(
    counts: &HashMap<String, i32>,
    client: &T,
) -> Result<(), String> {
    if counts.is_empty() {
        return Ok(());
    }
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
    for (key, value) in counts {
        params.push(key);
        params.push(value);
    }
    client
        .query(
            &format!(
                "INSERT INTO counts_by_type (type, count) VALUES {}
                ON CONFLICT (type) DO UPDATE SET count = counts_by_type.count + EXCLUDED.count",
                utils::multi_row_query_param_str(counts.len(), 2)
            ),
            &params,
        )
        .await
        .map_err(|e| format!("update_inscription_type_counts: {e}"))?;
    Ok(())
}

async fn update_genesis_address_counts<T: GenericClient>(
    counts: &HashMap<String, i32>,
    client: &T,
) -> Result<(), String> {
    if counts.is_empty() {
        return Ok(());
    }
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
    for (key, value) in counts {
        params.push(key);
        params.push(value);
    }
    client
        .query(
            &format!(
                "INSERT INTO counts_by_genesis_address (address, count) VALUES {}
                ON CONFLICT (address) DO UPDATE SET count = counts_by_genesis_address.count + EXCLUDED.count",
                utils::multi_row_query_param_str(counts.len(), 2)
            ),
            &params,
        )
        .await
        .map_err(|e| format!("update_genesis_address_counts: {e}"))?;
    Ok(())
}

async fn update_recursive_counts<T: GenericClient>(
    counts: &HashMap<bool, i32>,
    client: &T,
) -> Result<(), String> {
    if counts.is_empty() {
        return Ok(());
    }
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
    for (key, value) in counts {
        params.push(key);
        params.push(value);
    }
    client
        .query(
            &format!(
                "INSERT INTO counts_by_recursive (recursive, count) VALUES {}
                ON CONFLICT (recursive) DO UPDATE SET count = counts_by_recursive.count + EXCLUDED.count",
                utils::multi_row_query_param_str(counts.len(), 2)
            ),
            &params,
        )
        .await
        .map_err(|e| format!("update_recursive_counts: {e}"))?;
    Ok(())
}

async fn update_counts_by_block<T: GenericClient>(
    block_height: u64,
    block_hash: &String,
    inscription_count: usize,
    timestamp: u32,
    client: &T,
) -> Result<(), String> {
    if inscription_count == 0 {
        return Ok(());
    }
    client
        .query(
        "WITH prev_entry AS (
                SELECT inscription_count_accum
                FROM counts_by_block
                WHERE block_height < $1
                ORDER BY block_height DESC
                LIMIT 1
            )
            INSERT INTO counts_by_block (block_height, block_hash, inscription_count, inscription_count_accum, timestamp)
            VALUES ($1, $2, $3, COALESCE((SELECT inscription_count_accum FROM prev_entry), 0) + $3, $4)",
            &[&PgNumericU64(block_height), block_hash, &(inscription_count as i32), &PgBigIntU32(timestamp)],
        )
        .await
        .map_err(|e| format!("update_counts_by_block: {e}"))?;
    Ok(())
}

pub async fn update_chain_tip<T: GenericClient>(
    chain_tip: &BlockIdentifier,
    client: &T,
) -> Result<(), String> {
    client
        .query(
            "UPDATE chain_tip SET block_height = $1, block_hash = $2",
            &[
                &PgNumericU64(chain_tip.index),
                &chain_tip.hash[2..].to_string(),
            ],
        )
        .await
        .map_err(|e| format!("update_chain_tip: {e}"))?;
    Ok(())
}

/// Inserts an indexed ordinals block into the DB.
pub async fn insert_block<T: GenericClient>(
    block: &DogecoinBlockData,
    client: &T,
) -> Result<(), String> {
    let mut satoshis = vec![];
    let mut inscriptions = vec![];
    let mut locations = vec![];
    let mut inscription_recursions = vec![];
    let mut inscription_parents = vec![];
    let mut current_locations: HashMap<PgNumericU64, DbCurrentLocation> = HashMap::new();
    let mut mime_type_counts = HashMap::new();
    let mut sat_rarity_counts = HashMap::new();
    let mut inscription_type_counts = HashMap::new();
    let mut genesis_address_counts = HashMap::new();
    let mut recursive_counts = HashMap::new();

    let mut update_current_location =
        |ordinal_number: PgNumericU64, new_location: DbCurrentLocation| match current_locations
            .get(&ordinal_number)
        {
            Some(current_location) => {
                if new_location.block_height > current_location.block_height
                    || (new_location.block_height == current_location.block_height
                        && new_location.tx_index > current_location.tx_index)
                {
                    current_locations.insert(ordinal_number, new_location);
                }
            }
            None => {
                current_locations.insert(ordinal_number, new_location);
            }
        };
    for (tx_index, tx) in block.transactions.iter().enumerate() {
        for operation in tx.metadata.ordinal_operations.iter() {
            match operation {
                OrdinalOperation::InscriptionRevealed(reveal) => {
                    let mut inscription = DbInscription::from_reveal(
                        reveal,
                        &block.block_identifier,
                        &tx.transaction_identifier,
                        tx_index,
                        block.timestamp,
                    );
                    let mime_type = inscription.mime_type.clone();
                    let genesis_address = inscription.address.clone();
                    let recursions = DbInscriptionRecursion::from_reveal(reveal)?;
                    let is_recursive = !recursions.is_empty();
                    if is_recursive {
                        inscription.recursive = true;
                    }
                    inscription_recursions.extend(recursions);
                    inscription_parents.extend(DbInscriptionParent::from_reveal(reveal)?);
                    inscriptions.push(inscription);
                    locations.push(DbLocation::from_reveal(
                        reveal,
                        &block.block_identifier,
                        &tx.transaction_identifier,
                        tx_index,
                        block.timestamp,
                    ));
                    let satoshi = DbKoinu::from_reveal(reveal);
                    let rarity = satoshi.rarity.clone();
                    satoshis.push(satoshi);
                    update_current_location(
                        PgNumericU64(reveal.ordinal_number),
                        DbCurrentLocation::from_reveal(
                            reveal,
                            &block.block_identifier,
                            &tx.transaction_identifier,
                            tx_index,
                        ),
                    );
                    let inscription_type = if reveal.inscription_number.classic < 0 {
                        "cursed".to_string()
                    } else {
                        "blessed".to_string()
                    };
                    mime_type_counts
                        .entry(mime_type)
                        .and_modify(|c| *c += 1)
                        .or_insert(1);
                    sat_rarity_counts
                        .entry(rarity)
                        .and_modify(|c| *c += 1)
                        .or_insert(1);
                    inscription_type_counts
                        .entry(inscription_type)
                        .and_modify(|c| *c += 1)
                        .or_insert(1);
                    if let Some(genesis_address) = genesis_address {
                        genesis_address_counts
                            .entry(genesis_address)
                            .and_modify(|c| *c += 1)
                            .or_insert(1);
                    }
                    recursive_counts
                        .entry(is_recursive)
                        .and_modify(|c| *c += 1)
                        .or_insert(1);
                }
                OrdinalOperation::InscriptionTransferred(transfer) => {
                    locations.push(DbLocation::from_transfer(
                        transfer,
                        &block.block_identifier,
                        &tx.transaction_identifier,
                        tx_index,
                        block.timestamp,
                    ));
                    update_current_location(
                        PgNumericU64(transfer.ordinal_number),
                        DbCurrentLocation::from_transfer(
                            transfer,
                            &block.block_identifier,
                            &tx.transaction_identifier,
                            tx_index,
                        ),
                    );
                }
            }
        }
    }

    insert_inscriptions(&inscriptions, client).await?;
    insert_inscription_recursions(&inscription_recursions, client).await?;
    insert_inscription_parents(&inscription_parents, client).await?;
    insert_locations(&locations, client).await?;
    insert_koinus(&satoshis, client).await?;
    insert_current_locations(&current_locations, client).await?;
    update_mime_type_counts(&mime_type_counts, client).await?;
    update_sat_rarity_counts(&sat_rarity_counts, client).await?;
    update_inscription_type_counts(&inscription_type_counts, client).await?;
    update_genesis_address_counts(&genesis_address_counts, client).await?;
    update_recursive_counts(&recursive_counts, client).await?;
    update_counts_by_block(
        block.block_identifier.index,
        &block.block_identifier.hash[2..].to_string(),
        inscriptions.len(),
        block.timestamp,
        client,
    )
    .await?;
    update_chain_tip(&block.block_identifier, client).await?;

    Ok(())
}

/// Rolls back a previously-indexed block. It is the responsibility of the caller to make sure `block_height` is the last block
/// that was indexed.
pub async fn rollback_block<T: GenericClient>(block_height: u64, client: &T) -> Result<(), String> {
    // Delete previous current locations, deduct owner counts, remove orphaned sats
    let moved_sat_rows = client
        .query(
            "WITH affected_sats AS (
                SELECT ordinal_number FROM locations WHERE block_height = $1
            ),
            affected_owners AS (
                SELECT address, COUNT(*) AS count FROM locations WHERE block_height = $1 GROUP BY address
            ),
            address_count_updates AS (
                UPDATE counts_by_address SET count = (
                    SELECT counts_by_address.count - affected_owners.count
                    FROM affected_owners
                    WHERE affected_owners.address = counts_by_address.address
                )
                WHERE EXISTS (SELECT 1 FROM affected_owners WHERE affected_owners.address = counts_by_address.address)
            ),
            satoshi_deletes AS (
                DELETE FROM satoshis WHERE ordinal_number IN (
                    SELECT ordinal_number FROM affected_sats WHERE NOT EXISTS
                    (
                        SELECT 1 FROM inscriptions AS i
                        WHERE i.ordinal_number = affected_sats.ordinal_number AND i.block_height < $1
                    )
                )
                RETURNING ordinal_number, rarity
            ),
            deleted_satoshi_rarity AS (
                SELECT rarity, COUNT(*) FROM satoshi_deletes GROUP BY rarity
            ),
            rarity_count_updates AS (
                UPDATE counts_by_sat_rarity SET count = (
                    SELECT counts_by_sat_rarity.count - count
                    FROM deleted_satoshi_rarity
                    WHERE deleted_satoshi_rarity.rarity = counts_by_sat_rarity.rarity
                )
                WHERE EXISTS (SELECT 1 FROM deleted_satoshi_rarity WHERE deleted_satoshi_rarity.rarity = counts_by_sat_rarity.rarity)
            ),
            current_location_deletes AS (
                DELETE FROM current_locations WHERE ordinal_number IN (SELECT ordinal_number FROM affected_sats)
            )
            SELECT ordinal_number FROM affected_sats",
            &[&PgNumericU64(block_height)],
        )
        .await
        .map_err(|e| format!("rollback_block (1): {e}"))?;
    // Delete inscriptions and locations
    client
        .execute(
            "WITH transfer_deletes AS (DELETE FROM inscription_transfers WHERE block_height = $1),
            inscription_deletes AS (
                DELETE FROM inscriptions WHERE block_height = $1 RETURNING mime_type, classic_number, address, recursive
            ),
            inscription_delete_types AS (
                SELECT 'cursed' AS type, COUNT(*) AS count
                FROM inscription_deletes WHERE classic_number < 0
                UNION
                SELECT 'blessed' AS type, COUNT(*) AS count
                FROM inscription_deletes WHERE classic_number >= 0
            ),
            counts_by_block_deletes AS (DELETE FROM counts_by_block WHERE block_height = $1),
            type_count_updates AS (
                UPDATE counts_by_type SET count = (
                    SELECT counts_by_type.count - count
                    FROM inscription_delete_types
                    WHERE inscription_delete_types.type = counts_by_type.type
                )
                WHERE EXISTS (SELECT 1 FROM inscription_delete_types WHERE inscription_delete_types.type = counts_by_type.type)
            ),
            mime_type_count_updates AS (
                UPDATE counts_by_mime_type SET count = (
                    SELECT counts_by_mime_type.count - COUNT(*)
                    FROM inscription_deletes
                    WHERE inscription_deletes.mime_type = counts_by_mime_type.mime_type
                    GROUP BY inscription_deletes.mime_type
                )
                WHERE EXISTS (SELECT 1 FROM inscription_deletes WHERE inscription_deletes.mime_type = counts_by_mime_type.mime_type)
            ),
            genesis_address_count_updates AS (
                UPDATE counts_by_genesis_address SET count = (
                    SELECT counts_by_genesis_address.count - COUNT(*)
                    FROM inscription_deletes
                    WHERE inscription_deletes.address = counts_by_genesis_address.address
                    GROUP BY inscription_deletes.address
                )
                WHERE EXISTS (SELECT 1 FROM inscription_deletes WHERE inscription_deletes.address = counts_by_genesis_address.address)
            ),
            recursive_count_updates AS (
                UPDATE counts_by_recursive SET count = (
                    SELECT counts_by_recursive.count - COUNT(*)
                    FROM inscription_deletes
                    WHERE inscription_deletes.recursive = counts_by_recursive.recursive
                    GROUP BY inscription_deletes.recursive
                )
                WHERE EXISTS (SELECT 1 FROM inscription_deletes WHERE inscription_deletes.recursive = counts_by_recursive.recursive)
            )
            DELETE FROM locations WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .map_err(|e| format!("rollback_block (2): {e}"))?;
    // Re-compute current location and owners
    let moved_sats: Vec<PgNumericU64> = moved_sat_rows
        .iter()
        .map(|r| r.get("ordinal_number"))
        .collect();
    client
        .execute(
            "INSERT INTO current_locations (ordinal_number, block_height, tx_id, tx_index, address, output, \"offset\")
            (
                SELECT DISTINCT ON(ordinal_number) ordinal_number, block_height, tx_id, tx_index, address, output, \"offset\"
                FROM locations
                WHERE ordinal_number = ANY ($1)
                ORDER BY ordinal_number, block_height DESC, tx_index DESC
            )",
            &[&moved_sats]
        )
        .await
        .map_err(|e| format!("rollback_block (3): {e}"))?;
    client
        .execute(
            "WITH new_owners AS (
                SELECT address, COUNT(*) AS count
                FROM current_locations
                WHERE ordinal_number = ANY ($1)
                GROUP BY address
            )
            INSERT INTO counts_by_address (address, count)
            (SELECT address, count FROM new_owners)
            ON CONFLICT (address) DO UPDATE SET count = counts_by_address.count + EXCLUDED.count",
            &[&moved_sats],
        )
        .await
        .map_err(|e| format!("rollback_block (4): {e}"))?;
    client
        .execute(
            "WITH last_block AS (
                SELECT block_height, block_hash
                FROM locations
                ORDER BY block_height DESC
                LIMIT 1
            )
            UPDATE chain_tip SET
                block_height = (SELECT block_height FROM last_block),
                block_hash = (SELECT block_hash FROM last_block)",
            &[],
        )
        .await
        .map_err(|e| format!("rollback_block (5): {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// DNS — Dogecoin Name System
// ---------------------------------------------------------------------------

/// Insert DNS name registrations discovered in a block.
///
/// `dns_map`: name → inscription_id (first wins within a block).
/// Across blocks, `ON CONFLICT DO NOTHING` enforces first-wins semantics.
pub async fn insert_dns_names<T: GenericClient>(
    dns_map: &HashMap<String, String>,
    block_height: u64,
    block_timestamp: u32,
    client: &T,
) -> Result<(), String> {
    for (name, inscription_id) in dns_map {
        client
            .execute(
                "INSERT INTO dns_names (name, inscription_id, block_height, block_timestamp)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (name) DO NOTHING",
                &[
                    name,
                    inscription_id,
                    &(block_height as i64),
                    &(block_timestamp as i64),
                ],
            )
            .await
            .map_err(|e| format!("insert_dns_names: {e}"))?;
    }
    Ok(())
}

pub async fn rollback_dns_names<T: GenericClient>(
    block_height: u64,
    client: &T,
) -> Result<(), String> {
    client
        .execute(
            "DELETE FROM dns_names WHERE block_height = $1",
            &[&(block_height as i64)],
        )
        .await
        .map_err(|e| format!("rollback_dns_names: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Dogemap — block claim indexing
// ---------------------------------------------------------------------------

/// Insert Dogemap claims discovered in a block.
///
/// `dogemap_map`: block_number → inscription_id (first wins within a block).
/// Across blocks, `ON CONFLICT DO NOTHING` enforces first-wins semantics.
pub async fn insert_dogemap_claims<T: GenericClient>(
    dogemap_map: &HashMap<u32, String>,
    claim_height: u64,
    claim_timestamp: u32,
    client: &T,
) -> Result<(), String> {
    for (block_number, inscription_id) in dogemap_map {
        client
            .execute(
                "INSERT INTO dogemap_claims (block_number, inscription_id, claim_height, claim_timestamp)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (block_number) DO NOTHING",
                &[
                    &(*block_number as i64),
                    inscription_id,
                    &(claim_height as i64),
                    &(claim_timestamp as i64),
                ],
            )
            .await
            .map_err(|e| format!("insert_dogemap_claims: {e}"))?;
    }
    Ok(())
}

pub async fn rollback_dogemap_claims<T: GenericClient>(
    claim_height: u64,
    client: &T,
) -> Result<(), String> {
    client
        .execute(
            "DELETE FROM dogemap_claims WHERE claim_height = $1",
            &[&(claim_height as i64)],
        )
        .await
        .map_err(|e| format!("rollback_dogemap_claims: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// DNS query helpers
// ---------------------------------------------------------------------------

pub struct DnsNameRow {
    pub name: String,
    pub inscription_id: String,
    pub block_height: u64,
    pub block_timestamp: u64,
}

pub async fn get_dns_name<T: GenericClient>(
    name: &str,
    client: &T,
) -> Result<Option<DnsNameRow>, String> {
    let row = client
        .query_opt(
            "SELECT name, inscription_id, block_height, block_timestamp
             FROM dns_names WHERE name = $1",
            &[&name],
        )
        .await
        .map_err(|e| format!("get_dns_name: {e}"))?;
    Ok(row.map(|r| DnsNameRow {
        name: r.get(0),
        inscription_id: r.get(1),
        block_height: r.get::<_, i64>(2) as u64,
        block_timestamp: r.get::<_, i64>(3) as u64,
    }))
}

/// List all DNS names, optionally filtered by namespace (e.g. "doge").
/// Ordered by block_height ascending (first registered first).
pub async fn list_dns_names<T: GenericClient>(
    namespace: Option<&str>,
    limit: usize,
    offset: usize,
    client: &T,
) -> Result<Vec<DnsNameRow>, String> {
    let rows = match namespace {
        Some(ns) => {
            let pattern = format!("%.{}", ns);
            client
                .query(
                    "SELECT name, inscription_id, block_height, block_timestamp
                     FROM dns_names WHERE name LIKE $1
                     ORDER BY block_height ASC
                     LIMIT $2 OFFSET $3",
                    &[&pattern, &(limit as i64), &(offset as i64)],
                )
                .await
                .map_err(|e| format!("list_dns_names (namespace): {e}"))?
        }
        None => client
            .query(
                "SELECT name, inscription_id, block_height, block_timestamp
                     FROM dns_names
                     ORDER BY block_height ASC
                     LIMIT $1 OFFSET $2",
                &[&(limit as i64), &(offset as i64)],
            )
            .await
            .map_err(|e| format!("list_dns_names: {e}"))?,
    };
    Ok(rows
        .into_iter()
        .map(|r| DnsNameRow {
            name: r.get(0),
            inscription_id: r.get(1),
            block_height: r.get::<_, i64>(2) as u64,
            block_timestamp: r.get::<_, i64>(3) as u64,
        })
        .collect())
}

pub async fn count_dns_names<T: GenericClient>(client: &T) -> Result<i64, String> {
    let row = client
        .query_one("SELECT COUNT(*) FROM dns_names", &[])
        .await
        .map_err(|e| format!("count_dns_names: {e}"))?;
    Ok(row.get(0))
}

// ---------------------------------------------------------------------------
// Dogemap query helpers
// ---------------------------------------------------------------------------

pub struct DogemapClaimRow {
    pub block_number: u64,
    pub inscription_id: String,
    pub claim_height: u64,
    pub claim_timestamp: u64,
}

pub async fn get_dogemap_claim<T: GenericClient>(
    block_number: u32,
    client: &T,
) -> Result<Option<DogemapClaimRow>, String> {
    let row = client
        .query_opt(
            "SELECT block_number, inscription_id, claim_height, claim_timestamp
             FROM dogemap_claims WHERE block_number = $1",
            &[&(block_number as i64)],
        )
        .await
        .map_err(|e| format!("get_dogemap_claim: {e}"))?;
    Ok(row.map(|r| DogemapClaimRow {
        block_number: r.get::<_, i64>(0) as u64,
        inscription_id: r.get(1),
        claim_height: r.get::<_, i64>(2) as u64,
        claim_timestamp: r.get::<_, i64>(3) as u64,
    }))
}

/// List claimed Dogemap blocks, ordered by block_number ascending.
pub async fn list_dogemap_claims<T: GenericClient>(
    limit: usize,
    offset: usize,
    client: &T,
) -> Result<Vec<DogemapClaimRow>, String> {
    let rows = client
        .query(
            "SELECT block_number, inscription_id, claim_height, claim_timestamp
             FROM dogemap_claims
             ORDER BY block_number ASC
             LIMIT $1 OFFSET $2",
            &[&(limit as i64), &(offset as i64)],
        )
        .await
        .map_err(|e| format!("list_dogemap_claims: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|r| DogemapClaimRow {
            block_number: r.get::<_, i64>(0) as u64,
            inscription_id: r.get(1),
            claim_height: r.get::<_, i64>(2) as u64,
            claim_timestamp: r.get::<_, i64>(3) as u64,
        })
        .collect())
}

pub async fn count_dogemap_claims<T: GenericClient>(client: &T) -> Result<i64, String> {
    let row = client
        .query_one("SELECT COUNT(*) FROM dogemap_claims", &[])
        .await
        .map_err(|e| format!("count_dogemap_claims: {e}"))?;
    Ok(row.get(0))
}

// ---------------------------------------------------------------------------
// doge-lotto
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LottoWinnerRow {
    pub lotto_id: String,
    pub inscription_id: String,
    pub ticket_id: String,
    pub resolved_height: u64,
    pub rank: u32,
    pub score: u64,
    pub payout_bps: u32,
    pub gross_payout_koinu: u64,
    pub tip_percent: u8,
    pub tip_deduction_koinu: u64,
    pub payout_koinu: u64,
    pub seed_numbers: Vec<u16>,
    pub drawn_numbers: Vec<u16>,
    pub bonus_drawn_numbers: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct LottoTicketRow {
    pub lotto_id: String,
    pub inscription_id: String,
    pub ticket_id: String,
    pub tx_id: String,
    pub minted_height: u64,
    pub minted_timestamp: u64,
    pub seed_numbers: Vec<u16>,
    pub tip_percent: u8,
}

#[derive(Debug, Clone)]
pub struct StoredLottoRow {
    lotto_id: String,
    template: LottoTemplate,
    draw_block: u64,
    cutoff_block: u64,
    ticket_price_koinu: u64,
    prize_pool_address: String,
    fee_percent: u8,
    main_numbers: NumberConfig,
    bonus_numbers: NumberConfig,
    resolution_mode: ResolutionMode,
    rollover_enabled: bool,
    guaranteed_min_prize_koinu: Option<u64>,
}

#[derive(Debug, Clone)]
struct StoredTicketRow {
    inscription_id: String,
    ticket_id: String,
    seed_numbers: Vec<u16>,
    minted_height: u64,
    tip_percent: u8,
}

impl StoredLottoRow {
    fn as_deploy(&self) -> LottoDeploy {
        LottoDeploy {
            lotto_id: self.lotto_id.clone(),
            template: self.template.clone(),
            draw_block: self.draw_block,
            cutoff_block: self.cutoff_block,
            ticket_price_koinu: self.ticket_price_koinu,
            prize_pool_address: self.prize_pool_address.clone(),
            fee_percent: self.fee_percent,
            main_numbers: self.main_numbers.clone(),
            bonus_numbers: self.bonus_numbers.clone(),
            resolution_mode: self.resolution_mode.clone(),
            rollover_enabled: self.rollover_enabled,
            guaranteed_min_prize_koinu: self.guaranteed_min_prize_koinu,
        }
    }
}

pub async fn insert_lotto_lotteries<T: GenericClient>(
    lotto_deploy_map: &HashMap<String, ParsedLottoDeploy>,
    deploy_height: u64,
    deploy_timestamp: u32,
    client: &T,
) -> Result<(), String> {
    for parsed in lotto_deploy_map.values() {
        if parsed.deploy.draw_block <= deploy_height {
            continue;
        }
        if special_lotto_requires_zero_fee(&parsed.deploy.lotto_id)
            && parsed.deploy.fee_percent != 0
        {
            continue;
        }

        client
            .execute(
                "INSERT INTO lotto_lotteries (
                    lotto_id, inscription_id, deploy_tx_id, deploy_height, deploy_timestamp,
                          template, draw_block, cutoff_block, ticket_price_koinu, prize_pool_address, fee_percent,
                    main_numbers_pick, main_numbers_max, bonus_numbers_pick, bonus_numbers_max,
                    resolution_mode, rollover_enabled, guaranteed_min_prize_koinu
                 ) VALUES (
                    $1, $2, $3, $4, $5,
                          $6, $7, $8, $9, $10, $11,
                          $12, $13, $14, $15,
                          $16, $17, $18
                 )
                 ON CONFLICT (lotto_id) DO NOTHING",
                &[
                    &parsed.deploy.lotto_id,
                    &parsed.inscription_id,
                    &parsed.tx_id,
                    &(deploy_height as i64),
                    &(deploy_timestamp as i64),
                    &lotto_template_as_str(&parsed.deploy.template),
                    &(parsed.deploy.draw_block as i64),
                    &(parsed.deploy.cutoff_block as i64),
                    &(parsed.deploy.ticket_price_koinu as i64),
                    &parsed.deploy.prize_pool_address,
                    &(parsed.deploy.fee_percent as i32),
                    &(parsed.deploy.main_numbers.pick as i32),
                    &(parsed.deploy.main_numbers.max as i32),
                    &(parsed.deploy.bonus_numbers.pick as i32),
                    &(parsed.deploy.bonus_numbers.max as i32),
                    &resolution_mode_as_str(&parsed.deploy.resolution_mode),
                    &parsed.deploy.rollover_enabled,
                    &parsed
                        .deploy
                        .guaranteed_min_prize_koinu
                        .map(|value| value as i64),
                ],
            )
            .await
            .map_err(|e| format!("insert_lotto_lotteries: {e}"))?;
    }

    Ok(())
}

pub async fn insert_lotto_tickets<T: GenericClient>(
    lotto_mints: &[ParsedLottoMint],
    minted_height: u64,
    minted_timestamp: u32,
    protocol_dev_address: &str,
    client: &T,
) -> Result<Vec<LottoTicketRow>, String> {
    let mut inserted = Vec::new();
    for parsed in lotto_mints {
        let Some(lottery) = get_stored_lotto(&parsed.mint.lotto_id, client).await? else {
            continue;
        };
        let deploy = lottery.as_deploy();

        if minted_height > lottery.cutoff_block {
            continue;
        }
        if !validate_mint_against_deploy(&parsed.mint, &deploy) {
            continue;
        }

        let (payment_ok, _reason) = verify_lotto_payment(parsed, &deploy, protocol_dev_address);
        if !payment_ok {
            continue;
        }

        let seed_numbers = seed_numbers_to_i32(&parsed.mint.seed_numbers);
        let inserted_row = client
            .query_opt(
                "INSERT INTO lotto_tickets (
                          inscription_id, lotto_id, ticket_id, tx_id, minted_height, minted_timestamp, seed_numbers, tip_percent
                      ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT DO NOTHING
                 RETURNING inscription_id",
                &[
                    &parsed.inscription_id,
                    &parsed.mint.lotto_id,
                    &parsed.mint.ticket_id,
                    &parsed.tx_id,
                    &(minted_height as i64),
                    &(minted_timestamp as i64),
                    &seed_numbers,
                    &(parsed.mint.tip_percent as i32),
                ],
            )
            .await
            .map_err(|e| format!("insert_lotto_tickets: {e}"))?;

        if inserted_row.is_some() {
            inserted.push(LottoTicketRow {
                lotto_id: parsed.mint.lotto_id.clone(),
                inscription_id: parsed.inscription_id.clone(),
                ticket_id: parsed.mint.ticket_id.clone(),
                tx_id: parsed.tx_id.clone(),
                minted_height,
                minted_timestamp: minted_timestamp as u64,
                seed_numbers: parsed.mint.seed_numbers.clone(),
                tip_percent: parsed.mint.tip_percent,
            });
        }
    }

    Ok(inserted)
}

pub async fn get_lotto_deploy_by_id<T: GenericClient>(
    lotto_id: &str,
    client: &T,
) -> Result<Option<LottoDeploy>, String> {
    Ok(get_stored_lotto(lotto_id, client)
        .await?
        .map(|lotto| lotto.as_deploy()))
}

/// Resolve lottery winners at draw block.
///
/// UNCLAIMED PRIZE POLICY:
/// Winners have 30 days after the draw block to claim prizes by transferring their
/// winning ticket inscription to their desired address. Any prizes that remain unclaimed
/// after 30 days are permanently considered donations to the protocol developers.
///
/// For protocol-level lotteries (doge-69-420, doge-max), the prize_pool_address is
/// managed by the protocol developers, and unclaimed funds support ongoing development.
///
/// For community/mini lotteries, deployers manage their own prize_pool_address and
/// unclaimed funds according to their stated rules (30-day window recommended).
pub async fn resolve_lotto<T: GenericClient>(
    resolved_height: u64,
    resolved_block_hash: &str,
    resolved_timestamp: u32,
    client: &T,
) -> Result<Vec<LottoWinnerRow>, String> {
    let rows = client
        .query(
                "SELECT lotto_id, template, draw_block, cutoff_block, ticket_price_koinu, prize_pool_address, fee_percent,
                    main_numbers_pick, main_numbers_max, bonus_numbers_pick, bonus_numbers_max,
                    resolution_mode, rollover_enabled, guaranteed_min_prize_koinu
             FROM lotto_lotteries
             WHERE resolved = FALSE AND draw_block + 1 = $1
             ORDER BY deploy_height ASC, lotto_id ASC",
            &[&(resolved_height as i64)],
        )
        .await
        .map_err(|e| format!("resolve_lotto (load lotteries): {e}"))?;

    let mut resolved_winners = Vec::new();
    for row in rows {
        let lottery = stored_lotto_from_row(&row)?;
        let tickets =
            get_lotto_tickets_for_resolution(&lottery.lotto_id, lottery.draw_block, client).await?;
        let draw = derive_draw_for_deploy(resolved_block_hash, &lottery.as_deploy());
        let verified_ticket_count = tickets.len() as u64;
        let verified_sales_koinu = verified_ticket_count.saturating_mul(lottery.ticket_price_koinu);
        let fee_koinu = verified_sales_koinu.saturating_mul(lottery.fee_percent as u64) / 100;
        let mut net_prize_koinu = verified_sales_koinu.saturating_sub(fee_koinu);
        if let Some(minimum) = lottery.guaranteed_min_prize_koinu {
            net_prize_koinu = net_prize_koinu.max(minimum);
        }

        let (winner_rows, rollover_occurred) = resolve_lottery_winners(
            &lottery,
            &tickets,
            &draw,
            resolved_block_hash,
            resolved_height,
            net_prize_koinu,
        );

        client
            .execute(
                "UPDATE lotto_lotteries
                 SET resolved = TRUE,
                     resolved_height = $2,
                     resolved_timestamp = $3,
                     resolved_block_hash = $4,
                     drawn_numbers = $5,
                     bonus_drawn_numbers = $6,
                     verified_ticket_count = $7,
                     verified_sales_koinu = $8,
                     fee_koinu = $9,
                     net_prize_koinu = $10,
                     rollover_occurred = $11
                 WHERE lotto_id = $1",
                &[
                    &lottery.lotto_id,
                    &(resolved_height as i64),
                    &(resolved_timestamp as i64),
                    &resolved_block_hash.trim_start_matches("0x"),
                    &seed_numbers_to_i32(&draw.main_numbers),
                    &seed_numbers_to_i32(&draw.bonus_numbers),
                    &(verified_ticket_count as i64),
                    &(verified_sales_koinu as i64),
                    &(fee_koinu as i64),
                    &(net_prize_koinu as i64),
                    &rollover_occurred,
                ],
            )
            .await
            .map_err(|e| format!("resolve_lotto (update lottery): {e}"))?;

        for winner in &winner_rows {
            // Winners are recorded on-chain. They have 30 days to claim by transferring
            // their ticket inscription. Unclaimed prizes support protocol development.
            // If a mint committed a tip_percent, the matching payout deduction is
            // transparently persisted in tip_percent and tip_deduction_koinu.
            client
                .execute(
                    "INSERT INTO lotto_winners (
                        lotto_id, inscription_id, ticket_id, resolved_height,
                        rank, score, payout_bps, gross_payout_koinu, tip_percent, tip_deduction_koinu,
                        payout_koinu, seed_numbers, drawn_numbers, bonus_drawn_numbers
                     ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                     ON CONFLICT (lotto_id, inscription_id) DO NOTHING",
                    &[
                        &winner.lotto_id,
                        &winner.inscription_id,
                        &winner.ticket_id,
                        &(winner.resolved_height as i64),
                        &(winner.rank as i32),
                        &(winner.score as i64),
                        &(winner.payout_bps as i32),
                        &(winner.gross_payout_koinu as i64),
                        &(winner.tip_percent as i32),
                        &(winner.tip_deduction_koinu as i64),
                        &(winner.payout_koinu as i64),
                        &seed_numbers_to_i32(&winner.seed_numbers),
                        &seed_numbers_to_i32(&winner.drawn_numbers),
                        &seed_numbers_to_i32(&winner.bonus_drawn_numbers),
                    ],
                )
                .await
                .map_err(|e| format!("resolve_lotto (insert winners): {e}"))?;
        }

        resolved_winners.extend(winner_rows);
    }

    Ok(resolved_winners)
}

pub async fn rollback_lotto_lotteries<T: GenericClient>(
    deploy_height: u64,
    client: &T,
) -> Result<(), String> {
    client
        .execute(
            "DELETE FROM lotto_lotteries WHERE deploy_height = $1",
            &[&(deploy_height as i64)],
        )
        .await
        .map_err(|e| format!("rollback_lotto_lotteries: {e}"))?;
    Ok(())
}

pub async fn rollback_lotto_tickets<T: GenericClient>(
    minted_height: u64,
    client: &T,
) -> Result<(), String> {
    client
        .execute(
            "DELETE FROM lotto_tickets WHERE minted_height = $1",
            &[&(minted_height as i64)],
        )
        .await
        .map_err(|e| format!("rollback_lotto_tickets: {e}"))?;
    Ok(())
}

pub async fn rollback_lotto_resolutions<T: GenericClient>(
    resolved_height: u64,
    client: &T,
) -> Result<(), String> {
    client
        .execute(
            "DELETE FROM lotto_winners WHERE resolved_height = $1",
            &[&(resolved_height as i64)],
        )
        .await
        .map_err(|e| format!("rollback_lotto_resolutions (delete winners): {e}"))?;

    client
        .execute(
            "UPDATE lotto_lotteries
             SET resolved = FALSE,
                 resolved_height = NULL,
                 resolved_timestamp = NULL,
                 resolved_block_hash = NULL,
                 drawn_numbers = NULL,
                 bonus_drawn_numbers = ARRAY[]::INTEGER[],
                 verified_ticket_count = NULL,
                 verified_sales_koinu = NULL,
                 fee_koinu = NULL,
                 net_prize_koinu = NULL,
                 rollover_occurred = FALSE
             WHERE resolved_height = $1",
            &[&(resolved_height as i64)],
        )
        .await
        .map_err(|e| format!("rollback_lotto_resolutions (reset lotteries): {e}"))?;

    Ok(())
}

pub async fn rollback_lotto_burns<T: GenericClient>(
    block_height: u64,
    client: &T,
) -> Result<(), String> {
    // Get all burn events at this block to reverse burn points
    let burn_events = client
        .query(
            "SELECT owner_address FROM lotto_burn_events WHERE burn_height = $1",
            &[&(block_height as i64)],
        )
        .await
        .map_err(|e| format!("rollback_lotto_burns (SELECT): {e}"))?;

    // Decrement burn points for each burned ticket
    for row in burn_events {
        let owner_address: String = row.get(0);
        client
            .execute(
                "UPDATE lotto_burn_points
                 SET burn_points = GREATEST(burn_points - 1, 0),
                     total_tickets_burned = GREATEST(total_tickets_burned - 1, 0)
                 WHERE owner_address = $1",
                &[&owner_address],
            )
            .await
            .map_err(|e| format!("rollback_lotto_burns (UPDATE points): {e}"))?;
    }

    // Delete burn events
    client
        .execute(
            "DELETE FROM lotto_burn_events WHERE burn_height = $1",
            &[&(block_height as i64)],
        )
        .await
        .map_err(|e| format!("rollback_lotto_burns (DELETE events): {e}"))?;

    Ok(())
}

pub struct LottoSummaryRow {
    pub lotto_id: String,
    pub inscription_id: String,
    pub deploy_height: u64,
    pub deploy_timestamp: u64,
    pub template: String,
    pub draw_block: u64,
    pub cutoff_block: u64,
    pub ticket_price_koinu: u64,
    pub prize_pool_address: String,
    pub fee_percent: u8,
    pub main_numbers_pick: u16,
    pub main_numbers_max: u16,
    pub bonus_numbers_pick: u16,
    pub bonus_numbers_max: u16,
    pub resolution_mode: String,
    pub rollover_enabled: bool,
    pub guaranteed_min_prize_koinu: Option<u64>,
    pub resolved: bool,
    pub resolved_height: Option<u64>,
    pub drawn_numbers: Option<Vec<u16>>,
    pub bonus_drawn_numbers: Vec<u16>,
    pub verified_ticket_count: Option<u64>,
    pub verified_sales_koinu: Option<u64>,
    pub net_prize_koinu: Option<u64>,
    pub rollover_occurred: bool,
    pub current_ticket_count: u64,
}

pub struct LottoStatusRow {
    pub summary: LottoSummaryRow,
    pub winners: Vec<LottoWinnerRow>,
}

#[derive(Debug, Clone)]
pub struct LottoTicketCardRow {
    pub inscription_id: String,
    pub lotto_id: String,
    pub ticket_id: String,
    pub tx_id: String,
    pub minted_height: u64,
    pub minted_timestamp: u64,
    pub seed_numbers: Vec<u16>,
    pub tip_percent: u8,
}

pub async fn get_lotto_lottery<T: GenericClient>(
    lotto_id: &str,
    client: &T,
) -> Result<Option<LottoStatusRow>, String> {
    let row = client
        .query_opt(
                "SELECT l.lotto_id, l.inscription_id, l.deploy_height, l.deploy_timestamp, l.template, l.draw_block, l.cutoff_block,
                    l.ticket_price_koinu, l.prize_pool_address, l.fee_percent,
                    l.main_numbers_pick, l.main_numbers_max, l.bonus_numbers_pick, l.bonus_numbers_max,
                    l.resolution_mode, l.rollover_enabled, l.guaranteed_min_prize_koinu, l.resolved, l.resolved_height,
                    l.drawn_numbers, l.bonus_drawn_numbers,
                    l.verified_ticket_count, l.verified_sales_koinu, l.net_prize_koinu,
                    l.rollover_occurred,
                    COALESCE((SELECT COUNT(*) FROM lotto_tickets t WHERE t.lotto_id = l.lotto_id), 0) AS current_ticket_count
             FROM lotto_lotteries l
             WHERE l.lotto_id = $1",
            &[&lotto_id],
        )
        .await
        .map_err(|e| format!("get_lotto_lottery: {e}"))?;

    let Some(row) = row else {
        return Ok(None);
    };

    let winners = list_lotto_winners(lotto_id, client).await?;
    Ok(Some(LottoStatusRow {
        summary: lotto_summary_from_row(&row),
        winners,
    }))
}

pub async fn list_lotto_lotteries<T: GenericClient>(
    limit: usize,
    offset: usize,
    client: &T,
) -> Result<Vec<LottoSummaryRow>, String> {
    let rows = client
        .query(
                "SELECT l.lotto_id, l.inscription_id, l.deploy_height, l.deploy_timestamp, l.template, l.draw_block, l.cutoff_block,
                    l.ticket_price_koinu, l.prize_pool_address, l.fee_percent,
                    l.main_numbers_pick, l.main_numbers_max, l.bonus_numbers_pick, l.bonus_numbers_max,
                    l.resolution_mode, l.rollover_enabled, l.guaranteed_min_prize_koinu, l.resolved, l.resolved_height,
                    l.drawn_numbers, l.bonus_drawn_numbers,
                    l.verified_ticket_count, l.verified_sales_koinu, l.net_prize_koinu,
                    l.rollover_occurred,
                    COALESCE((SELECT COUNT(*) FROM lotto_tickets t WHERE t.lotto_id = l.lotto_id), 0) AS current_ticket_count
             FROM lotto_lotteries l
             ORDER BY l.deploy_height DESC, l.lotto_id ASC
             LIMIT $1 OFFSET $2",
            &[&(limit as i64), &(offset as i64)],
        )
        .await
        .map_err(|e| format!("list_lotto_lotteries: {e}"))?;

    Ok(rows
        .into_iter()
        .map(|row| lotto_summary_from_row(&row))
        .collect())
}

pub async fn list_lotto_tickets<T: GenericClient>(
    lotto_id: &str,
    limit: usize,
    offset: usize,
    client: &T,
) -> Result<Vec<LottoTicketCardRow>, String> {
    let rows = client
        .query(
            "SELECT inscription_id, lotto_id, ticket_id, tx_id, minted_height, minted_timestamp, seed_numbers, tip_percent
             FROM lotto_tickets
             WHERE lotto_id = $1
             ORDER BY minted_height DESC, inscription_id DESC
             LIMIT $2 OFFSET $3",
            &[&lotto_id, &(limit as i64), &(offset as i64)],
        )
        .await
        .map_err(|e| format!("list_lotto_tickets: {e}"))?;

    Ok(rows
        .into_iter()
        .map(|r| LottoTicketCardRow {
            inscription_id: r.get(0),
            lotto_id: r.get(1),
            ticket_id: r.get(2),
            tx_id: r.get(3),
            minted_height: r.get::<_, i64>(4) as u64,
            minted_timestamp: r.get::<_, i64>(5) as u64,
            seed_numbers: r
                .get::<_, Vec<i32>>(6)
                .into_iter()
                .map(|v| v as u16)
                .collect(),
            tip_percent: r.get::<_, i32>(7) as u8,
        })
        .collect())
}

pub async fn count_lotto_lotteries<T: GenericClient>(client: &T) -> Result<i64, String> {
    let row = client
        .query_one("SELECT COUNT(*) FROM lotto_lotteries", &[])
        .await
        .map_err(|e| format!("count_lotto_lotteries: {e}"))?;
    Ok(row.get(0))
}

pub async fn list_lotto_winners<T: GenericClient>(
    lotto_id: &str,
    client: &T,
) -> Result<Vec<LottoWinnerRow>, String> {
    let rows = client
        .query(
            "SELECT lotto_id, inscription_id, ticket_id, resolved_height, rank, score,
                    payout_bps, gross_payout_koinu, tip_percent, tip_deduction_koinu,
                    payout_koinu, seed_numbers, drawn_numbers, bonus_drawn_numbers
             FROM lotto_winners
             WHERE lotto_id = $1
             ORDER BY rank ASC, inscription_id ASC",
            &[&lotto_id],
        )
        .await
        .map_err(|e| format!("list_lotto_winners: {e}"))?;

    rows.into_iter()
        .map(|row| {
            Ok(LottoWinnerRow {
                lotto_id: row.get("lotto_id"),
                inscription_id: row.get("inscription_id"),
                ticket_id: row.get("ticket_id"),
                resolved_height: row.get::<_, i64>("resolved_height") as u64,
                rank: row.get::<_, i32>("rank") as u32,
                score: row.get::<_, i64>("score") as u64,
                payout_bps: row.get::<_, i32>("payout_bps") as u32,
                gross_payout_koinu: row.get::<_, i64>("gross_payout_koinu") as u64,
                tip_percent: row.get::<_, i32>("tip_percent") as u8,
                tip_deduction_koinu: row.get::<_, i64>("tip_deduction_koinu") as u64,
                payout_koinu: row.get::<_, i64>("payout_koinu") as u64,
                seed_numbers: i32_seed_numbers_to_u16(row.get("seed_numbers"))?,
                drawn_numbers: i32_seed_numbers_to_u16(row.get("drawn_numbers"))?,
                bonus_drawn_numbers: i32_seed_numbers_to_u16(row.get("bonus_drawn_numbers"))?,
            })
        })
        .collect()
}

pub async fn get_stored_lotto<T: GenericClient>(
    lotto_id: &str,
    client: &T,
) -> Result<Option<StoredLottoRow>, String> {
    let row = client
        .query_opt(
                "SELECT lotto_id, template, draw_block, cutoff_block, ticket_price_koinu, prize_pool_address, fee_percent,
                    main_numbers_pick, main_numbers_max, bonus_numbers_pick, bonus_numbers_max,
                    resolution_mode, rollover_enabled, guaranteed_min_prize_koinu
             FROM lotto_lotteries
             WHERE lotto_id = $1",
            &[&lotto_id],
        )
        .await
        .map_err(|e| format!("get_stored_lotto: {e}"))?;

    row.map(|row| stored_lotto_from_row(&row)).transpose()
}

async fn get_lotto_tickets_for_resolution<T: GenericClient>(
    lotto_id: &str,
    draw_block: u64,
    client: &T,
) -> Result<Vec<StoredTicketRow>, String> {
    let rows = client
        .query(
            "SELECT inscription_id, ticket_id, seed_numbers, minted_height
                    , tip_percent
             FROM lotto_tickets
             WHERE lotto_id = $1 AND minted_height <= $2
             ORDER BY minted_height ASC, inscription_id ASC",
            &[&lotto_id, &(draw_block as i64)],
        )
        .await
        .map_err(|e| format!("get_lotto_tickets_for_resolution: {e}"))?;

    rows.into_iter()
        .map(|row| {
            let seed_numbers: Vec<i32> = row.get("seed_numbers");
            Ok(StoredTicketRow {
                inscription_id: row.get("inscription_id"),
                ticket_id: row.get("ticket_id"),
                seed_numbers: i32_seed_numbers_to_u16(seed_numbers)?,
                minted_height: row.get::<_, i64>("minted_height") as u64,
                tip_percent: row.get::<_, i32>("tip_percent") as u8,
            })
        })
        .collect()
}

fn stored_lotto_from_row(row: &tokio_postgres::Row) -> Result<StoredLottoRow, String> {
    Ok(StoredLottoRow {
        lotto_id: row.get("lotto_id"),
        template: lotto_template_from_str(row.get::<_, String>("template").as_str())?,
        draw_block: row.get::<_, i64>("draw_block") as u64,
        cutoff_block: row.get::<_, i64>("cutoff_block") as u64,
        ticket_price_koinu: row.get::<_, i64>("ticket_price_koinu") as u64,
        prize_pool_address: row.get("prize_pool_address"),
        fee_percent: row.get::<_, i32>("fee_percent") as u8,
        main_numbers: NumberConfig {
            pick: row.get::<_, i32>("main_numbers_pick") as u16,
            max: row.get::<_, i32>("main_numbers_max") as u16,
        },
        bonus_numbers: NumberConfig {
            pick: row.get::<_, i32>("bonus_numbers_pick") as u16,
            max: row.get::<_, i32>("bonus_numbers_max") as u16,
        },
        resolution_mode: resolution_mode_from_str(
            row.get::<_, String>("resolution_mode").as_str(),
        )?,
        rollover_enabled: row.get("rollover_enabled"),
        guaranteed_min_prize_koinu: row
            .get::<_, Option<i64>>("guaranteed_min_prize_koinu")
            .map(|value| value as u64),
    })
}

fn lotto_summary_from_row(row: &tokio_postgres::Row) -> LottoSummaryRow {
    LottoSummaryRow {
        lotto_id: row.get("lotto_id"),
        inscription_id: row.get("inscription_id"),
        deploy_height: row.get::<_, i64>("deploy_height") as u64,
        deploy_timestamp: row.get::<_, i64>("deploy_timestamp") as u64,
        template: row.get("template"),
        draw_block: row.get::<_, i64>("draw_block") as u64,
        cutoff_block: row.get::<_, i64>("cutoff_block") as u64,
        ticket_price_koinu: row.get::<_, i64>("ticket_price_koinu") as u64,
        prize_pool_address: row.get("prize_pool_address"),
        fee_percent: row.get::<_, i32>("fee_percent") as u8,
        main_numbers_pick: row.get::<_, i32>("main_numbers_pick") as u16,
        main_numbers_max: row.get::<_, i32>("main_numbers_max") as u16,
        bonus_numbers_pick: row.get::<_, i32>("bonus_numbers_pick") as u16,
        bonus_numbers_max: row.get::<_, i32>("bonus_numbers_max") as u16,
        resolution_mode: row.get("resolution_mode"),
        rollover_enabled: row.get("rollover_enabled"),
        guaranteed_min_prize_koinu: row
            .get::<_, Option<i64>>("guaranteed_min_prize_koinu")
            .map(|value| value as u64),
        resolved: row.get("resolved"),
        resolved_height: row
            .get::<_, Option<i64>>("resolved_height")
            .map(|value| value as u64),
        drawn_numbers: row
            .get::<_, Option<Vec<i32>>>("drawn_numbers")
            .map(|numbers| i32_seed_numbers_to_u16(numbers).unwrap_or_default()),
        bonus_drawn_numbers: i32_seed_numbers_to_u16(row.get("bonus_drawn_numbers"))
            .unwrap_or_default(),
        verified_ticket_count: row
            .get::<_, Option<i64>>("verified_ticket_count")
            .map(|value| value as u64),
        verified_sales_koinu: row
            .get::<_, Option<i64>>("verified_sales_koinu")
            .map(|value| value as u64),
        net_prize_koinu: row
            .get::<_, Option<i64>>("net_prize_koinu")
            .map(|value| value as u64),
        rollover_occurred: row.get("rollover_occurred"),
        current_ticket_count: row.get::<_, i64>("current_ticket_count") as u64,
    }
}

fn resolve_lottery_winners(
    lottery: &StoredLottoRow,
    tickets: &[StoredTicketRow],
    draw: &LottoDraw,
    resolved_block_hash: &str,
    resolved_height: u64,
    net_prize_koinu: u64,
) -> (Vec<LottoWinnerRow>, bool) {
    match lottery.resolution_mode {
        ResolutionMode::AlwaysWinner => {
            if tickets.is_empty() {
                return (Vec::new(), false);
            }
            let scored = score_tickets(
                tickets,
                draw,
                &lottery.template,
                &lottery.lotto_id,
                resolved_block_hash,
            );
            let Some(best_score) = scored.first().map(|ticket| ticket.score) else {
                return (Vec::new(), false);
            };
            let best_bonus_score = scored
                .iter()
                .filter(|ticket| ticket.score == best_score)
                .map(|ticket| ticket.bonus_score)
                .min()
                .unwrap_or(0);
            let winners: Vec<_> = scored
                .into_iter()
                .filter(|ticket| {
                    ticket.score == best_score && ticket.bonus_score == best_bonus_score
                })
                .collect();
            let payouts = split_amount(net_prize_koinu, winners.len());
            (
                winners
                    .into_iter()
                    .zip(payouts)
                    .map(|(ticket, payout_koinu)| {
                        winner_from_scored_ticket(
                            &lottery.lotto_id,
                            resolved_height,
                            1,
                            10_000,
                            payout_koinu,
                            ticket,
                            draw,
                        )
                    })
                    .collect(),
                false,
            )
        }
        ResolutionMode::ClosestWins => {
            if tickets.is_empty() {
                return (Vec::new(), false);
            }
            let mut scored = score_tickets(
                tickets,
                draw,
                &lottery.template,
                &lottery.lotto_id,
                resolved_block_hash,
            );
            let mut payout_bps = payout_bps_for_template(&lottery.template, &lottery.lotto_id);
            let winner_cap = payout_bps.len().max(1);
            scored.truncate(winner_cap);
            payout_bps.truncate(scored.len());
            let allocated: u32 = payout_bps.iter().copied().sum();
            if let Some(first_share) = payout_bps.first_mut() {
                *first_share += 10_000_u32.saturating_sub(allocated);
            }
            let payouts = split_by_bps(net_prize_koinu, &payout_bps);
            (
                scored
                    .into_iter()
                    .zip(payout_bps)
                    .zip(payouts)
                    .enumerate()
                    .map(|(index, ((ticket, bps), payout_koinu))| {
                        winner_from_scored_ticket(
                            &lottery.lotto_id,
                            resolved_height,
                            (index + 1) as u32,
                            bps,
                            payout_koinu,
                            ticket,
                            draw,
                        )
                    })
                    .collect(),
                false,
            )
        }
        ResolutionMode::ExactOnlyWithRollover => {
            let exact_matches: Vec<_> = score_tickets(
                tickets,
                draw,
                &lottery.template,
                &lottery.lotto_id,
                resolved_block_hash,
            )
            .into_iter()
            .filter(|ticket| ticket.seed_numbers == draw.main_numbers)
            .collect();
            if exact_matches.is_empty() {
                return (Vec::new(), lottery.rollover_enabled);
            }
            let payouts = split_amount(net_prize_koinu, exact_matches.len());
            (
                exact_matches
                    .into_iter()
                    .zip(payouts)
                    .map(|(ticket, payout_koinu)| {
                        winner_from_scored_ticket(
                            &lottery.lotto_id,
                            resolved_height,
                            1,
                            10_000,
                            payout_koinu,
                            ticket,
                            draw,
                        )
                    })
                    .collect(),
                false,
            )
        }
    }
}

#[derive(Debug, Clone)]
struct ScoredTicketRow {
    inscription_id: String,
    ticket_id: String,
    seed_numbers: Vec<u16>,
    score: u64,
    bonus_score: u64,
    minted_height: u64,
    tip_percent: u8,
}

fn score_tickets(
    tickets: &[StoredTicketRow],
    draw: &LottoDraw,
    template: &LottoTemplate,
    lotto_id: &str,
    resolved_block_hash: &str,
) -> Vec<ScoredTicketRow> {
    let mut scored: Vec<_> = tickets
        .iter()
        .map(|ticket| ScoredTicketRow {
            inscription_id: ticket.inscription_id.clone(),
            ticket_id: ticket.ticket_id.clone(),
            seed_numbers: ticket.seed_numbers.clone(),
            score: ticket_distance_score(ticket, draw, lotto_id, resolved_block_hash),
            bonus_score: bonus_score_for_ticket(ticket, draw, template, lotto_id),
            minted_height: ticket.minted_height,
            tip_percent: ticket.tip_percent,
        })
        .collect();
    scored.sort_by(|left, right| {
        left.score
            .cmp(&right.score)
            .then_with(|| left.bonus_score.cmp(&right.bonus_score))
            .then_with(|| left.minted_height.cmp(&right.minted_height))
            .then_with(|| left.inscription_id.cmp(&right.inscription_id))
    });
    scored
}

fn winner_from_scored_ticket(
    lotto_id: &str,
    resolved_height: u64,
    rank: u32,
    payout_bps: u32,
    gross_payout_koinu: u64,
    ticket: ScoredTicketRow,
    draw: &LottoDraw,
) -> LottoWinnerRow {
    let tip_deduction_koinu = gross_payout_koinu.saturating_mul(ticket.tip_percent as u64) / 100;
    let payout_koinu = gross_payout_koinu.saturating_sub(tip_deduction_koinu);

    LottoWinnerRow {
        lotto_id: lotto_id.to_string(),
        inscription_id: ticket.inscription_id,
        ticket_id: ticket.ticket_id,
        resolved_height,
        rank,
        score: ticket.score,
        payout_bps,
        gross_payout_koinu,
        tip_percent: ticket.tip_percent,
        tip_deduction_koinu,
        payout_koinu,
        seed_numbers: ticket.seed_numbers,
        drawn_numbers: draw.main_numbers.clone(),
        bonus_drawn_numbers: draw.bonus_numbers.clone(),
    }
}

fn ticket_distance_score(
    ticket: &StoredTicketRow,
    draw: &LottoDraw,
    lotto_id: &str,
    resolved_block_hash: &str,
) -> u64 {
    if lotto_id == "doge-4-20-blaze" {
        return blaze_distance_score(&ticket.seed_numbers, resolved_block_hash);
    }
    score_ticket(&ticket.seed_numbers, &draw.main_numbers)
}

fn blaze_distance_score(seed_numbers: &[u16], resolved_block_hash: &str) -> u64 {
    let mut sorted = seed_numbers.to_vec();
    sorted.sort_unstable();
    let fingerprint_input = sorted
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let fingerprint = Sha256::digest(fingerprint_input.as_bytes());
    let target = hex::decode(resolved_block_hash.trim_start_matches("0x")).unwrap_or_default();
    if target.len() != 32 {
        return u64::MAX / 2;
    }
    let distance = abs_diff_be_32(&fingerprint.into(), &target.try_into().unwrap_or([0u8; 32]));
    u64::from_be_bytes(distance[0..8].try_into().unwrap_or([0u8; 8]))
}

fn abs_diff_be_32(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    if left >= right {
        sub_be_32(left, right)
    } else {
        sub_be_32(right, left)
    }
}

fn sub_be_32(large: &[u8; 32], small: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut value = large[i] as i16 - small[i] as i16 - borrow;
        if value < 0 {
            value += 256;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out[i] = value as u8;
    }
    out
}

fn split_amount(amount: u64, recipients: usize) -> Vec<u64> {
    if recipients == 0 {
        return Vec::new();
    }
    let base = amount / recipients as u64;
    let remainder = amount % recipients as u64;
    (0..recipients)
        .map(|index| base + if (index as u64) < remainder { 1 } else { 0 })
        .collect()
}

fn split_by_bps(amount: u64, payout_bps: &[u32]) -> Vec<u64> {
    if payout_bps.is_empty() {
        return Vec::new();
    }
    let mut payouts = Vec::with_capacity(payout_bps.len());
    let mut allocated = 0_u64;
    for (index, bps) in payout_bps.iter().enumerate() {
        if index + 1 == payout_bps.len() {
            payouts.push(amount.saturating_sub(allocated));
        } else {
            let payout = amount.saturating_mul(*bps as u64) / 10_000;
            payouts.push(payout);
            allocated = allocated.saturating_add(payout);
        }
    }
    payouts
}

fn payout_bps_for_template(template: &LottoTemplate, lotto_id: &str) -> Vec<u32> {
    if lotto_id == "doge-4-20-blaze" {
        return vec![6_500, 1_500, 1_000, 1_667, 1_666, 1_667];
    }
    match template {
        LottoTemplate::ClosestWins => vec![6_000, 2_500, 1_500],
        LottoTemplate::Six49Classic => vec![7_000, 2_000, 1_000],
        LottoTemplate::LifeAnnuity => vec![10_000],
        LottoTemplate::PowerballDualDrum => vec![8_500, 1_000, 500],
        LottoTemplate::RolloverJackpot | LottoTemplate::AlwaysWinner | LottoTemplate::Custom => {
            vec![6_000, 2_500, 1_500]
        }
    }
}

fn bonus_score_for_ticket(
    ticket: &StoredTicketRow,
    draw: &LottoDraw,
    template: &LottoTemplate,
    lotto_id: &str,
) -> u64 {
    if draw.bonus_numbers.is_empty() {
        return 0;
    }

    match template {
        LottoTemplate::PowerballDualDrum | LottoTemplate::Custom => {
            score_ticket(&ticket.seed_numbers, &draw.bonus_numbers)
        }
        LottoTemplate::ClosestWins if lotto_id == "doge-4-20-blaze" => {
            let matches = ticket
                .seed_numbers
                .iter()
                .filter(|n| draw.bonus_numbers.contains(n))
                .count() as u64;
            match matches {
                3 => 0,
                2 => 1,
                _ => 2,
            }
        }
        _ => 0,
    }
}

pub fn verify_lotto_payment(
    tx: &crate::core::protocol::inscription_parsing::ParsedLottoMint,
    deploy: &LottoDeploy,
    protocol_dev_address: &str,
) -> (bool, String) {
    // Atomic lotto mints are valid only when this same transaction pays the
    // deploy's prize pool exactly `ticket_price_koinu` and includes any committed
    // immutable tip payment to protocol_dev_address.
    let mut paid_prize_pool_koinu = 0_u64;
    let mut paid_protocol_dev_koinu = 0_u64;

    for output in &tx.outputs {
        let Some(script) = script_buf_from_hex(&output.script_pubkey) else {
            continue;
        };
        let Some(address) = dogecoin_address_from_script(&script) else {
            continue;
        };

        if address == deploy.prize_pool_address {
            paid_prize_pool_koinu = paid_prize_pool_koinu.saturating_add(output.value);
        }
        if !protocol_dev_address.is_empty() && address == protocol_dev_address {
            paid_protocol_dev_koinu = paid_protocol_dev_koinu.saturating_add(output.value);
        }
    }

    if paid_prize_pool_koinu != deploy.ticket_price_koinu {
        return (
            false,
            format!(
                "payment mismatch in tx {}: expected {} koinu to {}, found {}",
                tx.tx_id,
                deploy.ticket_price_koinu,
                deploy.prize_pool_address,
                paid_prize_pool_koinu
            ),
        );
    }

    let expected_tip_koinu = deploy
        .ticket_price_koinu
        .saturating_mul(tx.mint.tip_percent as u64)
        / 100;

    if expected_tip_koinu > 0 && protocol_dev_address.is_empty() {
        return (
            false,
            format!(
                "payment mismatch in tx {}: tip_percent={} requires protocol_dev_address",
                tx.tx_id, tx.mint.tip_percent
            ),
        );
    }

    if paid_protocol_dev_koinu != expected_tip_koinu {
        return (
            false,
            format!(
                "payment mismatch in tx {}: expected {} koinu tip to {}, found {}",
                tx.tx_id, expected_tip_koinu, protocol_dev_address, paid_protocol_dev_koinu
            ),
        );
    }

    (true, "payment verified".to_string())
}

fn script_buf_from_hex(script_pubkey: &str) -> Option<ScriptBuf> {
    let hex = script_pubkey.trim_start_matches("0x");
    let bytes = hex::decode(hex).ok()?;
    Some(ScriptBuf::from_bytes(bytes))
}

fn dogecoin_base58check(version: u8, payload: &[u8]) -> String {
    let mut data = Vec::with_capacity(1 + payload.len());
    data.push(version);
    data.extend_from_slice(payload);
    bitcoin::base58::encode_check(&data)
}

fn dogecoin_address_from_script(script: &ScriptBuf) -> Option<String> {
    let bytes = script.as_bytes();
    if script.is_p2pkh() && bytes.len() == 25 {
        Some(dogecoin_base58check(0x1e, &bytes[3..23]))
    } else if script.is_p2sh() && bytes.len() == 23 {
        Some(dogecoin_base58check(0x16, &bytes[2..22]))
    } else {
        None
    }
}

fn special_lotto_requires_zero_fee(lotto_id: &str) -> bool {
    matches!(lotto_id, "doge-69-420" | "doge-max")
}

fn lotto_template_as_str(template: &LottoTemplate) -> &'static str {
    match template {
        LottoTemplate::ClosestWins => "closest_wins",
        LottoTemplate::PowerballDualDrum => "powerball_dual_drum",
        LottoTemplate::Six49Classic => "6_49_classic",
        LottoTemplate::RolloverJackpot => "rollover_jackpot",
        LottoTemplate::AlwaysWinner => "always_winner",
        LottoTemplate::LifeAnnuity => "life_annuity",
        LottoTemplate::Custom => "custom",
    }
}

fn lotto_template_from_str(template: &str) -> Result<LottoTemplate, String> {
    match template {
        "closest_wins" => Ok(LottoTemplate::ClosestWins),
        "powerball_dual_drum" => Ok(LottoTemplate::PowerballDualDrum),
        "6_49_classic" => Ok(LottoTemplate::Six49Classic),
        "rollover_jackpot" => Ok(LottoTemplate::RolloverJackpot),
        "always_winner" => Ok(LottoTemplate::AlwaysWinner),
        "life_annuity" => Ok(LottoTemplate::LifeAnnuity),
        "custom" => Ok(LottoTemplate::Custom),
        other => Err(format!("unknown lotto template: {other}")),
    }
}

fn resolution_mode_as_str(mode: &ResolutionMode) -> &'static str {
    match mode {
        ResolutionMode::AlwaysWinner => "always_winner",
        ResolutionMode::ClosestWins => "closest_wins",
        ResolutionMode::ExactOnlyWithRollover => "exact_only_with_rollover",
    }
}

fn resolution_mode_from_str(mode: &str) -> Result<ResolutionMode, String> {
    match mode {
        "always_winner" => Ok(ResolutionMode::AlwaysWinner),
        "closest_wins" => Ok(ResolutionMode::ClosestWins),
        "exact_only_with_rollover" => Ok(ResolutionMode::ExactOnlyWithRollover),
        other => Err(format!("unknown lotto resolution mode: {other}")),
    }
}

fn seed_numbers_to_i32(seed_numbers: &[u16]) -> Vec<i32> {
    seed_numbers.iter().map(|number| *number as i32).collect()
}

fn i32_seed_numbers_to_u16(seed_numbers: Vec<i32>) -> Result<Vec<u16>, String> {
    seed_numbers
        .into_iter()
        .map(|number| {
            u16::try_from(number)
                .map_err(|_| format!("invalid lotto seed number stored in db: {number}"))
        })
        .collect()
}

// ===========================
// Burners: Burn Point tracking
// ===========================

/// Record a burn event: user sent expired ticket to burn address, earn 1 Burn Point.
pub async fn record_lotto_burn<T: GenericClient>(
    inscription_id: &str,
    lotto_id: &str,
    ticket_id: &str,
    owner_address: &str,
    burn_height: u64,
    burn_timestamp: u32,
    burn_tx_id: &str,
    client: &T,
) -> Result<(), String> {
    // Insert burn event
    client
        .execute(
            "INSERT INTO lotto_burn_events (inscription_id, lotto_id, ticket_id, owner_address, burn_height, burn_timestamp, burn_tx_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (inscription_id) DO NOTHING",
            &[
                &inscription_id,
                &lotto_id,
                &ticket_id,
                &owner_address,
                &(burn_height as i64),
                &(burn_timestamp as i64),
                &burn_tx_id,
            ],
        )
        .await
        .map_err(|e| format!("record_lotto_burn (event): {e}"))?;

    // Increment burn points for owner
    client
        .execute(
            "INSERT INTO lotto_burn_points (owner_address, burn_points, last_burn_height, last_burn_timestamp, total_tickets_burned)
             VALUES ($1, 1, $2, $3, 1)
             ON CONFLICT (owner_address) DO UPDATE SET
                burn_points = lotto_burn_points.burn_points + 1,
                last_burn_height = EXCLUDED.last_burn_height,
                last_burn_timestamp = EXCLUDED.last_burn_timestamp,
                total_tickets_burned = lotto_burn_points.total_tickets_burned + 1",
            &[
                &owner_address,
                &(burn_height as i64),
                &(burn_timestamp as i64),
            ],
        )
        .await
        .map_err(|e| format!("record_lotto_burn (points): {e}"))?;

    Ok(())
}

/// Get burn points for a specific address.
pub async fn get_burn_points<T: GenericClient>(
    owner_address: &str,
    client: &T,
) -> Result<Option<BurnPointsRow>, String> {
    let row = client
        .query_opt(
            "SELECT owner_address, burn_points, last_burn_height, last_burn_timestamp, total_tickets_burned
             FROM lotto_burn_points
             WHERE owner_address = $1",
            &[&owner_address],
        )
        .await
        .map_err(|e| format!("get_burn_points: {e}"))?;

    match row {
        Some(r) => Ok(Some(BurnPointsRow {
            owner_address: r.get(0),
            burn_points: r.get::<_, i64>(1) as u64,
            last_burn_height: r.get::<_, Option<i64>>(2).map(|h| h as u64),
            last_burn_timestamp: r.get::<_, Option<i64>>(3).map(|ts| ts as u64),
            total_tickets_burned: r.get::<_, i64>(4) as u64,
        })),
        None => Ok(None),
    }
}

/// Get top burners leaderboard.
pub async fn get_top_burners<T: GenericClient>(
    limit: usize,
    client: &T,
) -> Result<Vec<BurnPointsRow>, String> {
    let rows = client
        .query(
            "SELECT owner_address, burn_points, last_burn_height, last_burn_timestamp, total_tickets_burned
             FROM lotto_burn_points
             ORDER BY burn_points DESC
             LIMIT $1",
            &[&(limit as i64)],
        )
        .await
        .map_err(|e| format!("get_top_burners: {e}"))?;

    Ok(rows
        .into_iter()
        .map(|r| BurnPointsRow {
            owner_address: r.get(0),
            burn_points: r.get::<_, i64>(1) as u64,
            last_burn_height: r.get::<_, Option<i64>>(2).map(|h| h as u64),
            last_burn_timestamp: r.get::<_, Option<i64>>(3).map(|ts| ts as u64),
            total_tickets_burned: r.get::<_, i64>(4) as u64,
        })
        .collect())
}

/// Get lotto ticket info by inscription_id (for burn detection).
pub async fn get_lotto_ticket_by_inscription<T: GenericClient>(
    inscription_id: &str,
    client: &T,
) -> Result<Option<LottoTicketInfoRow>, String> {
    let row = client
        .query_opt(
            "SELECT lotto_id, ticket_id
             FROM lotto_tickets
             WHERE inscription_id = $1",
            &[&inscription_id],
        )
        .await
        .map_err(|e| format!("get_lotto_ticket_by_inscription: {e}"))?;

    match row {
        Some(r) => Ok(Some(LottoTicketInfoRow {
            lotto_id: r.get(0),
            ticket_id: r.get(1),
        })),
        None => Ok(None),
    }
}

#[derive(Debug, Clone)]
pub struct BurnPointsRow {
    pub owner_address: String,
    pub burn_points: u64,
    pub last_burn_height: Option<u64>,
    pub last_burn_timestamp: Option<u64>,
    pub total_tickets_burned: u64,
}

#[derive(Debug, Clone)]
pub struct LottoTicketInfoRow {
    pub lotto_id: String,
    pub ticket_id: String,
}

// ---------------------------------------------------------------------------
// Dogetag — on-chain graffiti indexing
// ---------------------------------------------------------------------------

/// Insert all Dogetag messages discovered in a block.
/// `tags`: list of (txid, sender_address, message, raw_script).
pub async fn insert_dogetags<T: GenericClient>(
    tags: &[(String, Option<String>, String, String)],
    block_height: u64,
    block_timestamp: u32,
    client: &T,
) -> Result<(), String> {
    for (txid, sender_address, message, raw_script) in tags {
        client
            .execute(
                "INSERT INTO dogetags
                    (txid, block_height, block_timestamp, sender_address, message, message_bytes, raw_script)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[
                    txid,
                    &(block_height as i64),
                    &(block_timestamp as i64),
                    sender_address,
                    message,
                    &(message.len() as i32),
                    raw_script,
                ],
            )
            .await
            .map_err(|e| format!("insert_dogetags: {e}"))?;
    }
    Ok(())
}

pub async fn rollback_dogetags<T: GenericClient>(
    block_height: u64,
    client: &T,
) -> Result<(), String> {
    client
        .execute(
            "DELETE FROM dogetags WHERE block_height = $1",
            &[&(block_height as i64)],
        )
        .await
        .map_err(|e| format!("rollback_dogetags: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Dogetag query helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DogetagRow {
    pub id: i64,
    pub txid: String,
    pub block_height: u64,
    pub block_timestamp: u64,
    pub sender_address: Option<String>,
    pub message: String,
    pub message_bytes: i32,
}

pub async fn list_dogetags<T: GenericClient>(
    limit: usize,
    offset: usize,
    client: &T,
) -> Result<Vec<DogetagRow>, String> {
    let rows = client
        .query(
            "SELECT id, txid, block_height, block_timestamp, sender_address, message, message_bytes
             FROM dogetags
             ORDER BY block_height DESC, id DESC
             LIMIT $1 OFFSET $2",
            &[&(limit as i64), &(offset as i64)],
        )
        .await
        .map_err(|e| format!("list_dogetags: {e}"))?;
    Ok(rows
        .iter()
        .map(|r| DogetagRow {
            id: r.get(0),
            txid: r.get(1),
            block_height: r.get::<_, i64>(2) as u64,
            block_timestamp: r.get::<_, i64>(3) as u64,
            sender_address: r.get(4),
            message: r.get(5),
            message_bytes: r.get(6),
        })
        .collect())
}

pub async fn search_dogetags<T: GenericClient>(
    query: &str,
    limit: usize,
    client: &T,
) -> Result<Vec<DogetagRow>, String> {
    let pattern = format!("%{}%", query);
    let rows = client
        .query(
            "SELECT id, txid, block_height, block_timestamp, sender_address, message, message_bytes
             FROM dogetags
             WHERE message ILIKE $1
             ORDER BY block_height DESC, id DESC
             LIMIT $2",
            &[&pattern, &(limit as i64)],
        )
        .await
        .map_err(|e| format!("search_dogetags: {e}"))?;
    Ok(rows
        .iter()
        .map(|r| DogetagRow {
            id: r.get(0),
            txid: r.get(1),
            block_height: r.get::<_, i64>(2) as u64,
            block_timestamp: r.get::<_, i64>(3) as u64,
            sender_address: r.get(4),
            message: r.get(5),
            message_bytes: r.get(6),
        })
        .collect())
}

pub async fn get_dogetags_by_address<T: GenericClient>(
    address: &str,
    limit: usize,
    client: &T,
) -> Result<Vec<DogetagRow>, String> {
    let rows = client
        .query(
            "SELECT id, txid, block_height, block_timestamp, sender_address, message, message_bytes
             FROM dogetags
             WHERE sender_address = $1
             ORDER BY block_height DESC, id DESC
             LIMIT $2",
            &[&address, &(limit as i64)],
        )
        .await
        .map_err(|e| format!("get_dogetags_by_address: {e}"))?;
    Ok(rows
        .iter()
        .map(|r| DogetagRow {
            id: r.get(0),
            txid: r.get(1),
            block_height: r.get::<_, i64>(2) as u64,
            block_timestamp: r.get::<_, i64>(3) as u64,
            sender_address: r.get(4),
            message: r.get(5),
            message_bytes: r.get(6),
        })
        .collect())
}

pub async fn count_dogetags<T: GenericClient>(client: &T) -> Result<i64, String> {
    let row = client
        .query_one("SELECT COUNT(*) FROM dogetags", &[])
        .await
        .map_err(|e| format!("count_dogetags: {e}"))?;
    Ok(row.get(0))
}

#[cfg(test)]
mod test {
    use deadpool_postgres::GenericClient;
    use dogecoin::types::{
        OrdinalInscriptionNumber, OrdinalInscriptionRevealData, OrdinalInscriptionTransferData,
        OrdinalInscriptionTransferDestination, OrdinalOperation,
    };
    use postgres::{
        pg_begin, pg_pool_client,
        types::{PgBigIntU32, PgNumericU64},
        FromPgRow,
    };

    use crate::{
        core::test_builders::{TestBlockBuilder, TestTransactionBuilder},
        db::{
            doginals_pg::{
                self, get_chain_tip_block_height, get_inscriptions_at_block, insert_block,
                rollback_block,
            },
            models::{DbCurrentLocation, DbInscription, DbKoinu, DbLocation},
            pg_reset_db, pg_test_connection, pg_test_connection_pool,
        },
    };

    async fn get_current_location<T: GenericClient>(
        ordinal_number: u64,
        client: &T,
    ) -> Option<DbCurrentLocation> {
        let row = client
            .query_opt(
                "SELECT * FROM current_locations WHERE ordinal_number = $1",
                &[&PgNumericU64(ordinal_number)],
            )
            .await
            .unwrap();
        row.map(|r| DbCurrentLocation::from_pg_row(&r))
    }

    async fn get_locations<T: GenericClient>(ordinal_number: u64, client: &T) -> Vec<DbLocation> {
        let row = client
            .query(
                "SELECT * FROM locations WHERE ordinal_number = $1",
                &[&PgNumericU64(ordinal_number)],
            )
            .await
            .unwrap();
        row.iter().map(|r| DbLocation::from_pg_row(&r)).collect()
    }

    async fn get_inscription<T: GenericClient>(
        inscription_id: &str,
        client: &T,
    ) -> Option<DbInscription> {
        let row = client
            .query_opt(
                "SELECT * FROM inscriptions WHERE inscription_id = $1",
                &[&inscription_id],
            )
            .await
            .unwrap();
        row.map(|r| DbInscription::from_pg_row(&r))
    }

    async fn get_satoshi<T: GenericClient>(ordinal_number: u64, client: &T) -> Option<DbKoinu> {
        let row = client
            .query_opt(
                "SELECT * FROM satoshis WHERE ordinal_number = $1",
                &[&PgNumericU64(ordinal_number)],
            )
            .await
            .unwrap();
        row.map(|r| DbKoinu::from_pg_row(&r))
    }

    async fn get_mime_type_count<T: GenericClient>(mime_type: &str, client: &T) -> i32 {
        let row = client
            .query_opt(
                "SELECT COALESCE(count, 0) AS count FROM counts_by_mime_type WHERE mime_type = $1",
                &[&mime_type],
            )
            .await
            .unwrap()
            .unwrap();
        let count: i32 = row.get("count");
        count
    }

    async fn get_sat_rarity_count<T: GenericClient>(rarity: &str, client: &T) -> i32 {
        let row = client
            .query_opt(
                "SELECT COALESCE(count, 0) AS count FROM counts_by_sat_rarity WHERE rarity = $1",
                &[&rarity],
            )
            .await
            .unwrap()
            .unwrap();
        let count: i32 = row.get("count");
        count
    }

    async fn get_type_count<T: GenericClient>(type_str: &str, client: &T) -> i32 {
        let row = client
            .query_opt(
                "SELECT COALESCE(count, 0) AS count FROM counts_by_type WHERE type = $1",
                &[&type_str],
            )
            .await
            .unwrap()
            .unwrap();
        let count: i32 = row.get("count");
        count
    }

    async fn get_address_count<T: GenericClient>(address: &str, client: &T) -> i32 {
        let row = client
            .query_opt(
                "SELECT COALESCE(count, 0) AS count FROM counts_by_address WHERE address = $1",
                &[&address],
            )
            .await
            .unwrap()
            .unwrap();
        let count: i32 = row.get("count");
        count
    }

    async fn get_genesis_address_count<T: GenericClient>(address: &str, client: &T) -> i32 {
        let row = client
            .query_opt(
                "SELECT COALESCE(count, 0) AS count FROM counts_by_genesis_address WHERE address = $1",
                &[&address],
            )
            .await
            .unwrap()
            .unwrap();
        let count: i32 = row.get("count");
        count
    }

    async fn get_recursive_count<T: GenericClient>(recursive: bool, client: &T) -> i32 {
        let row = client
            .query_opt(
                "SELECT COALESCE(count, 0) AS count FROM counts_by_recursive WHERE recursive = $1",
                &[&recursive],
            )
            .await
            .unwrap()
            .unwrap();
        let count: i32 = row.get("count");
        count
    }

    async fn get_block_reveal_count<T: GenericClient>(block_height: u64, client: &T) -> i32 {
        let row = client
            .query_opt(
                "SELECT COALESCE(inscription_count, 0) AS count FROM counts_by_block WHERE block_height = $1",
                &[&PgNumericU64(block_height)],
            )
            .await
            .unwrap();
        row.map(|r| r.get("count")).unwrap_or(0)
    }

    #[tokio::test]
    async fn test_apply_and_rollback() -> Result<(), String> {
        let mut pg_client = pg_test_connection().await;
        doginals_pg::migrate(&mut pg_client).await?;
        {
            let mut ord_client = pg_pool_client(&pg_test_connection_pool()).await?;
            let client = pg_begin(&mut ord_client).await?;
            // Reveal
            {
                let block = TestBlockBuilder::new()
                    .height(800000)
                    .hash("0x000000000000000000024d4c784521e54b6f4a5945376ae6e248cee1ed2c0627".to_string())
                    .add_transaction(
                        TestTransactionBuilder::new()
                            .hash("0xb61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735".to_string())
                            .add_ordinal_operation(OrdinalOperation::InscriptionRevealed(
                                OrdinalInscriptionRevealData {
                                    content_bytes: "0x7b200a20202270223a20226272632d3230222c0a2020226f70223a20226465706c6f79222c0a2020227469636b223a20226f726469222c0a2020226d6178223a20223231303030303030222c0a2020226c696d223a202231303030220a7d".to_string(),
                                    content_type: "text/plain;charset=utf-8".to_string(),
                                    content_length: 94,
                                    inscription_number: OrdinalInscriptionNumber { classic: 0, jubilee: 0 },
                                    inscription_fee: 0,
                                    inscription_output_value: 10000,
                                    inscription_id: "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735i0".to_string(),
                                    inscription_input_index: 0,
                                    inscription_pointer: None,
                                    inscriber_address: Some("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp".to_string()),
                                    delegate: None,
                                    metaprotocol: None,
                                    metadata: None,
                                    parents: vec![],
                                    ordinal_number: 7000,
                                    ordinal_block_height: 0,
                                    ordinal_offset: 0,
                                    tx_index: 0,
                                    transfers_pre_inscription: 0,
                                    koinupoint_post_inscription: "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735:0:0".to_string(),
                                    curse_type: None,
                                    charms: 0,
                                    unbound_sequence: None,
                                },
                            ))
                            .build()
                    )
                    .build();
                insert_block(&block, &client).await?;
                assert_eq!(1, get_inscriptions_at_block(&client, 800000).await?.len());
                assert!(get_inscription(
                    "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735i0",
                    &client
                )
                .await
                .is_some());
                let locations = get_locations(7000, &client).await;
                assert_eq!(1, locations.len());
                assert_eq!(
                    Some(&DbLocation {
                        ordinal_number: PgNumericU64(7000),
                        block_height: PgNumericU64(800000),
                        tx_id: "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735"
                            .to_string(),
                        tx_index: PgBigIntU32(0),
                        block_hash:
                            "000000000000000000024d4c784521e54b6f4a5945376ae6e248cee1ed2c0627"
                                .to_string(),
                        address: Some("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp".to_string()),
                        output:
                            "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735:0"
                                .to_string(),
                        offset: Some(PgNumericU64(0)),
                        prev_output: None,
                        prev_offset: None,
                        value: Some(PgNumericU64(10000)),
                        transfer_type: "transferred".to_string(),
                        timestamp: PgBigIntU32(1712982301)
                    }),
                    locations.get(0)
                );
                assert_eq!(
                    Some(DbCurrentLocation {
                        ordinal_number: PgNumericU64(7000),
                        block_height: PgNumericU64(800000),
                        tx_id: "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735"
                            .to_string(),
                        tx_index: PgBigIntU32(0),
                        address: Some("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp".to_string()),
                        output:
                            "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735:0"
                                .to_string(),
                        offset: Some(PgNumericU64(0))
                    }),
                    get_current_location(7000, &client).await
                );
                assert_eq!(
                    Some(DbKoinu {
                        ordinal_number: PgNumericU64(7000),
                        rarity: "common".to_string(),
                        coinbase_height: PgNumericU64(0)
                    }),
                    get_satoshi(7000, &client).await
                );
                assert_eq!(1, get_mime_type_count("text/plain", &client).await);
                assert_eq!(1, get_sat_rarity_count("common", &client).await);
                assert_eq!(1, get_recursive_count(false, &client).await);
                assert_eq!(
                    1,
                    get_address_count("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp", &client).await
                );
                assert_eq!(
                    1,
                    get_genesis_address_count("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp", &client).await
                );
                assert_eq!(1, get_type_count("blessed", &client).await);
                assert_eq!(1, get_block_reveal_count(800000, &client).await);
                assert_eq!(Some(800000), get_chain_tip_block_height(&client).await?);
            }
            // Transfer
            {
                let block = TestBlockBuilder::new()
                    .height(800001)
                    .hash("0x00000000000000000001b322ec2ea8b5b9b0ac413385069ad6b0c84e0331bf23".to_string())
                    .add_transaction(
                        TestTransactionBuilder::new()
                            .hash("0x4862db07b588ebfd8627371045d6d17a99a66a01759782d7dd3009f68adb860f".to_string())
                            .add_ordinal_operation(OrdinalOperation::InscriptionTransferred(
                                OrdinalInscriptionTransferData {
                                    ordinal_number: 7000,
                                    destination: OrdinalInscriptionTransferDestination::Transferred("3DnzPvLPH1jA9EqQzq3Fgo9BMDya4eG1ay".to_string()),
                                    koinupoint_pre_transfer: "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735:0:0".to_string(),
                                    koinupoint_post_transfer: "4862db07b588ebfd8627371045d6d17a99a66a01759782d7dd3009f68adb860f:0:0".to_string(),
                                    post_transfer_output_value: Some(8000),
                                    tx_index: 0
                                }
                            ))
                            .build()
                    )
                    .build();
                insert_block(&block, &client).await?;
                assert_eq!(0, get_inscriptions_at_block(&client, 800001).await?.len());
                let locations = get_locations(7000, &client).await;
                assert_eq!(2, locations.len());
                assert_eq!(
                    Some(&DbLocation {
                        ordinal_number: PgNumericU64(7000),
                        block_height: PgNumericU64(800001),
                        tx_id: "4862db07b588ebfd8627371045d6d17a99a66a01759782d7dd3009f68adb860f"
                            .to_string(),
                        tx_index: PgBigIntU32(0),
                        block_hash:
                            "00000000000000000001b322ec2ea8b5b9b0ac413385069ad6b0c84e0331bf23"
                                .to_string(),
                        address: Some("3DnzPvLPH1jA9EqQzq3Fgo9BMDya4eG1ay".to_string()),
                        output:
                            "4862db07b588ebfd8627371045d6d17a99a66a01759782d7dd3009f68adb860f:0"
                                .to_string(),
                        offset: Some(PgNumericU64(0)),
                        prev_output: Some(
                            "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735:0"
                                .to_string()
                        ),
                        prev_offset: Some(PgNumericU64(0)),
                        value: Some(PgNumericU64(8000)),
                        transfer_type: "transferred".to_string(),
                        timestamp: PgBigIntU32(1712982301)
                    }),
                    locations.get(1)
                );
                assert_eq!(
                    Some(DbCurrentLocation {
                        ordinal_number: PgNumericU64(7000),
                        block_height: PgNumericU64(800001),
                        tx_id: "4862db07b588ebfd8627371045d6d17a99a66a01759782d7dd3009f68adb860f"
                            .to_string(),
                        tx_index: PgBigIntU32(0),
                        address: Some("3DnzPvLPH1jA9EqQzq3Fgo9BMDya4eG1ay".to_string()),
                        output:
                            "4862db07b588ebfd8627371045d6d17a99a66a01759782d7dd3009f68adb860f:0"
                                .to_string(),
                        offset: Some(PgNumericU64(0))
                    }),
                    get_current_location(7000, &client).await
                );
                assert_eq!(1, get_mime_type_count("text/plain", &client).await);
                assert_eq!(1, get_sat_rarity_count("common", &client).await);
                assert_eq!(1, get_recursive_count(false, &client).await);
                assert_eq!(
                    0,
                    get_address_count("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp", &client).await
                );
                assert_eq!(
                    1,
                    get_address_count("3DnzPvLPH1jA9EqQzq3Fgo9BMDya4eG1ay", &client).await
                );
                assert_eq!(
                    1,
                    get_genesis_address_count("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp", &client).await
                );
                assert_eq!(1, get_type_count("blessed", &client).await);
                assert_eq!(Some(800001), get_chain_tip_block_height(&client).await?);
            }

            // Rollback transfer
            {
                rollback_block(800001, &client).await?;
                assert_eq!(1, get_locations(7000, &client).await.len());
                assert_eq!(
                    Some(DbCurrentLocation {
                        ordinal_number: PgNumericU64(7000),
                        block_height: PgNumericU64(800000),
                        tx_id: "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735"
                            .to_string(),
                        tx_index: PgBigIntU32(0),
                        address: Some("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp".to_string()),
                        output:
                            "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735:0"
                                .to_string(),
                        offset: Some(PgNumericU64(0))
                    }),
                    get_current_location(7000, &client).await
                );
                assert_eq!(1, get_mime_type_count("text/plain", &client).await);
                assert_eq!(1, get_sat_rarity_count("common", &client).await);
                assert_eq!(1, get_recursive_count(false, &client).await);
                assert_eq!(
                    0,
                    get_address_count("3DnzPvLPH1jA9EqQzq3Fgo9BMDya4eG1ay", &client).await
                );
                assert_eq!(
                    1,
                    get_address_count("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp", &client).await
                );
                assert_eq!(
                    1,
                    get_genesis_address_count("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp", &client).await
                );
                assert_eq!(1, get_type_count("blessed", &client).await);
                assert_eq!(1, get_block_reveal_count(800000, &client).await);
                assert_eq!(Some(800000), get_chain_tip_block_height(&client).await?);
            }
            // Rollback reveal
            {
                rollback_block(800000, &client).await?;
                assert_eq!(0, get_inscriptions_at_block(&client, 800000).await?.len());
                assert_eq!(
                    None,
                    get_inscription(
                        "b61b0172d95e266c18aea0c624db987e971a5d6d4ebc2aaed85da4642d635735i0",
                        &client
                    )
                    .await
                );
                assert_eq!(0, get_locations(7000, &client).await.len());
                assert_eq!(None, get_current_location(7000, &client).await);
                assert_eq!(None, get_satoshi(7000, &client).await);
                assert_eq!(0, get_mime_type_count("text/plain", &client).await);
                assert_eq!(0, get_recursive_count(false, &client).await);
                assert_eq!(
                    0,
                    get_address_count("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp", &client).await
                );
                assert_eq!(
                    0,
                    get_genesis_address_count("324A7GHA2azecbVBAFy4pzEhcPT1GjbUAp", &client).await
                );
                assert_eq!(0, get_type_count("blessed", &client).await);
                assert_eq!(0, get_block_reveal_count(800000, &client).await);
                assert_eq!(0, get_sat_rarity_count("common", &client).await);
                // We don't have a previous block so it goes to none.
                assert_eq!(None, get_chain_tip_block_height(&client).await?);
            }
        }
        pg_reset_db(&mut pg_client).await?;
        Ok(())
    }
}
