use std::{collections::HashMap, process, str::FromStr};

use crate::db::cache::input_dune_balance::InputDuneBalance;
use bitcoin::Txid;
use config::Config;
use deadpool_postgres::GenericClient;
use dogecoin::{try_error, try_info, types::BlockIdentifier, utils::Context};
use doginals_parser::DuneId;
use models::{
    db_balance_change::DbBalanceChange, db_ledger_entry::DbLedgerEntry, db_dune::DbDune,
    db_supply_change::DbSupplyChange,
};
use postgres::{
    pg_connect_with_retry,
    types::{PgBigIntU32, PgNumericU128, PgNumericU64},
};
use refinery::embed_migrations;
use tokio_postgres::{types::ToSql, Error, Transaction};

pub mod cache;
pub mod index;
pub mod models;

embed_migrations!("../../migrations/dunes");
pub async fn migrate(pg_client: &mut tokio_postgres::Client, ctx: &Context) {
    try_info!(ctx, "DunesDb running postgres migrations...");
    match migrations::runner()
        .set_abort_divergent(false)
        .set_abort_missing(false)
        .set_migration_table_name("pgmigrations")
        .run_async(pg_client)
        .await
    {
        Ok(_) => {
            try_info!(ctx, "DunesDb postgres migrations complete");
        }
        Err(e) => {
            try_error!(ctx, "DunesDb error running pg migrations: {e}");
            process::exit(1);
        }
    };
}

pub async fn run_migrations(config: &Config, ctx: &Context) {
    let mut pg_client = pg_connect_with_retry(&config.dunes.as_ref().unwrap().db).await;
    migrate(&mut pg_client, ctx).await;
}

pub async fn pg_insert_dunes(
    rows: &[DbDune],
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    for row in rows.iter() {
        let params: Vec<&(dyn ToSql + Sync)> = vec![
            &row.id,
            &row.name,
            &row.spaced_name,
            &row.block_hash,
            &row.block_height,
            &row.tx_index,
            &row.tx_id,
            &row.divisibility,
            &row.premine,
            &row.symbol,
            &row.terms_amount,
            &row.terms_cap,
            &row.terms_height_start,
            &row.terms_height_end,
            &row.terms_offset_start,
            &row.terms_offset_end,
            &row.turbo,
            &row.cenotaph,
            &row.timestamp,
        ];

        if let Err(e) = db_tx
            .query(
                "INSERT INTO dunes \
                   (id, number, name, spaced_name, block_hash, block_height, tx_index, tx_id, divisibility, premine, symbol, \
                    terms_amount, terms_cap, terms_height_start, terms_height_end, terms_offset_start, terms_offset_end, turbo, cenotaph, timestamp) \
                 SELECT \
                   $1, (SELECT COALESCE(MAX(number), 0) + 1 FROM dunes), $2, $3, $4, $5, $6, $7, $8, $9, $10, \
                   $11, $12, $13, $14, $15, $16, $17, $18, $19 \
                 WHERE NOT EXISTS (SELECT 1 FROM dunes WHERE name = $2) \
                 ON CONFLICT (name) DO NOTHING",
                &params,
            )
            .await
        {
            try_error!(ctx, "Error inserting dune: {:?}", e);
            process::exit(1);
        }
    }
    Ok(true)
}

pub async fn pg_insert_supply_changes(
    rows: &[DbSupplyChange],
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    for chunk in rows.chunks(500) {
        let mut arg_num = 1;
        let mut arg_str = String::new();
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            arg_str.push_str(
                format!(
                    "(${},${}::numeric,${}::numeric,${}::numeric,${}::numeric,${}::numeric,${}::numeric),",
                    arg_num,
                    arg_num + 1,
                    arg_num + 2,
                    arg_num + 3,
                    arg_num + 4,
                    arg_num + 5,
                    arg_num + 6
                )
                .as_str(),
            );
            arg_num += 7;
            params.push(&row.dune_id);
            params.push(&row.block_height);
            params.push(&row.minted);
            params.push(&row.total_mints);
            params.push(&row.burned);
            params.push(&row.total_burns);
            params.push(&row.total_operations);
        }
        arg_str.pop();
        match db_tx
            .query(
                &format!("
                WITH changes (dune_id, block_height, minted, total_mints, burned, total_burns, total_operations) AS (VALUES {}),
                previous AS (
                    SELECT DISTINCT ON (dune_id) *
                    FROM supply_changes
                    WHERE dune_id IN (SELECT dune_id FROM changes)
                    ORDER BY dune_id, block_height DESC
                ),
                inserts AS (
                    SELECT c.dune_id,
                        c.block_height,
                        COALESCE(p.minted, 0) + c.minted AS minted,
                        COALESCE(p.total_mints, 0) + c.total_mints AS total_mints,
                        COALESCE(p.burned, 0) + c.burned AS burned,
                        COALESCE(p.total_burns, 0) + c.total_burns AS total_burns,
                        COALESCE(p.total_operations, 0) + c.total_operations AS total_operations
                    FROM changes AS c
                    LEFT JOIN previous AS p ON c.dune_id = p.dune_id
                )
                INSERT INTO supply_changes (dune_id, block_height, minted, total_mints, burned, total_burns, total_operations)
                (SELECT * FROM inserts)
                ON CONFLICT (dune_id, block_height) DO UPDATE SET
                    minted = EXCLUDED.minted,
                    total_mints = EXCLUDED.total_mints,
                    burned = EXCLUDED.burned,
                    total_burns = EXCLUDED.total_burns,
                    total_operations = EXCLUDED.total_operations
                ", arg_str),
                &params,
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                try_error!(ctx, "Error inserting supply changes: {:?}", e);
                process::exit(1);
            }
        };
    }
    Ok(true)
}

pub async fn pg_insert_balance_changes(
    rows: &[DbBalanceChange],
    increase: bool,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    let sign = if increase { "+" } else { "-" };
    for chunk in rows.chunks(500) {
        let mut arg_num = 1;
        let mut arg_str = String::new();
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            arg_str.push_str(
                format!(
                    "(${},${}::numeric,${},${}::numeric,${}::bigint),",
                    arg_num,
                    arg_num + 1,
                    arg_num + 2,
                    arg_num + 3,
                    arg_num + 4
                )
                .as_str(),
            );
            arg_num += 5;
            params.push(&row.dune_id);
            params.push(&row.block_height);
            params.push(&row.address);
            params.push(&row.balance);
            params.push(&row.total_operations);
        }
        arg_str.pop();
        match db_tx
            .query(
                &format!("WITH changes (dune_id, block_height, address, balance, total_operations) AS (VALUES {}),
                previous AS (
                    SELECT DISTINCT ON (dune_id, address) *
                    FROM balance_changes
                    WHERE (dune_id, address) IN (SELECT dune_id, address FROM changes)
                    ORDER BY dune_id, address, block_height DESC
                ),
                inserts AS (
                    SELECT c.dune_id, c.block_height, c.address, COALESCE(p.balance, 0) {} c.balance AS balance,
                        COALESCE(p.total_operations, 0) + c.total_operations AS total_operations
                    FROM changes AS c
                    LEFT JOIN previous AS p ON c.dune_id = p.dune_id AND c.address = p.address
                )
                INSERT INTO balance_changes (dune_id, block_height, address, balance, total_operations)
                (SELECT * FROM inserts)
                ON CONFLICT (dune_id, block_height, address) DO UPDATE SET
                    balance = EXCLUDED.balance,
                    total_operations = EXCLUDED.total_operations", arg_str, sign),
                &params,
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                try_error!(ctx, "Error inserting balance changes: {:?}", e);
                process::exit(1);
            }
        };
    }
    Ok(true)
}

pub async fn pg_insert_ledger_entries(
    rows: &[DbLedgerEntry],
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    for chunk in rows.chunks(500) {
        let mut arg_num = 1;
        let mut arg_str = String::new();
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            arg_str.push('(');
            for i in 0..12 {
                arg_str.push_str(format!("${},", arg_num + i).as_str());
            }
            arg_str.pop();
            arg_str.push_str("),");
            arg_num += 12;
            params.push(&row.dune_id);
            params.push(&row.block_hash);
            params.push(&row.block_height);
            params.push(&row.tx_index);
            params.push(&row.event_index);
            params.push(&row.tx_id);
            params.push(&row.output);
            params.push(&row.address);
            params.push(&row.receiver_address);
            params.push(&row.amount);
            params.push(&row.operation);
            params.push(&row.timestamp);
        }
        arg_str.pop();
        match db_tx
            .query(
                &format!("INSERT INTO ledger
                    (dune_id, block_hash, block_height, tx_index, event_index, tx_id, output, address, receiver_address, amount,
                    operation, timestamp)
                    VALUES {}", arg_str),
                &params,
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                try_error!(ctx, "Error inserting ledger entries: {:?}", e);
                process::exit(1);
            }
        };
    }
    Ok(true)
}

pub async fn pg_roll_back_block(block_height: u64, db_tx: &mut Transaction<'_>, _ctx: &Context) {
    db_tx
        .execute(
            "DELETE FROM balance_changes WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .expect("error rolling back balance_changes");
    db_tx
        .execute(
            "DELETE FROM supply_changes WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .expect("error rolling back supply_changes");
    db_tx
        .execute(
            "DELETE FROM ledger WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .expect("error rolling back ledger");
    db_tx
        .execute(
            "DELETE FROM dunes WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .expect("error rolling back dunes");
}

pub async fn pg_get_max_dune_number<T: GenericClient>(client: &T) -> u32 {
    let row = client
        .query_opt("SELECT MAX(number) AS max FROM dunes", &[])
        .await
        .expect("error getting max dune number");
    let Some(row) = row else {
        return 0;
    };
    let max: PgBigIntU32 = row.get("max");
    max.0
}

pub async fn pg_get_block_height<T: GenericClient>(client: &T) -> Option<u64> {
    let row = client
        .query_opt("SELECT MAX(block_height) AS max FROM ledger", &[])
        .await
        .expect("error getting max block height")?;
    let max: Option<PgNumericU64> = row.get("max");
    max.map(|max| max.0)
}

pub async fn get_chain_tip<T: GenericClient>(client: &T) -> Option<BlockIdentifier> {
    let row = client
        .query_opt(
            "SELECT block_height, block_hash
            FROM ledger
            ORDER BY block_height DESC
            LIMIT 1",
            &[],
        )
        .await
        .expect("get_chain_tip");
    let row = row?;
    let block_height: PgNumericU64 = row.get("block_height");
    let block_hash: String = row.get("block_hash");
    Some(BlockIdentifier {
        index: block_height.0,
        hash: format!("0x{block_hash}"),
    })
}

pub async fn pg_get_dune_by_id(
    id: &DuneId,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Option<DbDune> {
    let row = match db_tx
        .query_opt("SELECT * FROM dunes WHERE id = $1", &[&id.to_string()])
        .await
    {
        Ok(row) => row,
        Err(e) => {
            try_error!(ctx, "error retrieving dune: {}", e.to_string());
            process::exit(1);
        }
    };
    let row = row?;
    Some(DbDune::from_pg_row(&row))
}

pub async fn pg_get_dune_total_mints(
    id: &DuneId,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Option<u128> {
    let row = match db_tx
        .query_opt(
            "SELECT total_mints FROM supply_changes WHERE dune_id = $1 ORDER BY block_height DESC LIMIT 1",
            &[&id.to_string()],
        )
        .await
    {
        Ok(row) => row,
        Err(e) => {
            try_error!(
                ctx,
                "error retrieving dune minted total: {}",
                e.to_string()
            );
            process::exit(1);
        }
    };
    let row = row?;
    let minted: PgNumericU128 = row.get("total_mints");
    Some(minted.0)
}

/// Retrieves the dune balance for an array of transaction inputs represented by `(vin, tx_id, vout)` where `vin` is the index of
/// this transaction input, `tx_id` is the transaction ID that produced this input and `vout` is the output index of this previous
/// tx.
pub async fn pg_get_input_dune_balances(
    outputs: Vec<(u32, String, u32)>,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> HashMap<u32, HashMap<DuneId, Vec<InputDuneBalance>>> {
    // Instead of preparing a statement and running it thousands of times, pull all rows with 1 query.
    let mut arg_num = 1;
    let mut args = String::new();
    let mut data = vec![];
    for (input_index, tx_id, output) in outputs.iter() {
        args.push_str(
            format!(
                "(${}::bigint,${},${}::bigint),",
                arg_num,
                arg_num + 1,
                arg_num + 2
            )
            .as_str(),
        );
        arg_num += 3;
        data.push((PgBigIntU32(*input_index), tx_id, PgBigIntU32(*output)));
    }
    args.pop();
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
    for d in data.iter() {
        params.push(&d.0);
        params.push(d.1);
        params.push(&d.2);
    }
    let rows = match db_tx
        .query(
            format!(
                "WITH inputs (index, tx_id, output) AS (VALUES {})
                SELECT i.index, l.dune_id, l.address, l.amount
                FROM ledger AS l
                INNER JOIN inputs AS i USING (tx_id, output)
                WHERE l.operation = 'receive'",
                args
            )
            .as_str(),
            &params,
        )
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            try_error!(
                ctx,
                "error retrieving output dune balances: {}",
                e.to_string()
            );
            process::exit(1);
        }
    };
    let mut results: HashMap<u32, HashMap<DuneId, Vec<InputDuneBalance>>> = HashMap::new();
    for row in rows.iter() {
        let key: PgBigIntU32 = row.get("index");
        let dune_str: String = row.get("dune_id");
        let dune_id = DuneId::from_str(dune_str.as_str()).unwrap();
        let address: Option<String> = row.get("address");
        let amount: PgNumericU128 = row.get("amount");
        let tx_id_str: String = row.get("tx_id");
        let vout: u32 = row.get("output");
        let input_bal = InputDuneBalance {
            dune_id: dune_id.clone(),
            balance: amount.0,
            txid: Txid::from_str(tx_id_str.as_str()).unwrap(),
            vout,
            address,
            block_height: 0,
            timestamp: 0,
        };
        if let Some(input) = results.get_mut(&key.0) {
            if let Some(dune_bal) = input.get_mut(&dune_id) {
                dune_bal.push(input_bal);
            } else {
                input.insert(dune_id, vec![input_bal]);
            }
        } else {
            let mut map = HashMap::new();
            map.insert(dune_id, vec![input_bal]);
            results.insert(key.0, map);
        }
    }
    results
}

#[cfg(test)]
pub fn pg_test_config() -> config::PgDatabaseConfig {
    config::PgDatabaseConfig {
        dbname: "postgres".to_string(),
        host: "localhost".to_string(),
        port: 5432,
        user: "postgres".to_string(),
        password: Some("postgres".to_string()),
        search_path: None,
        pool_max_size: None,
    }
}

#[cfg(test)]
pub async fn pg_test_client(run_migrations: bool, ctx: &Context) -> tokio_postgres::Client {
    let mut pg_client = postgres::pg_connect_with_retry(&pg_test_config()).await;
    if run_migrations {
        migrate(&mut pg_client, ctx).await;
    }
    pg_client
}

#[cfg(test)]
pub async fn pg_test_roll_back_migrations(pg_client: &mut tokio_postgres::Client, ctx: &Context) {
    match pg_client
        .batch_execute(
            "
            DO $$ DECLARE
                r RECORD;
            BEGIN
                FOR r IN (SELECT tablename FROM pg_tables WHERE schemaname = current_schema()) LOOP
                    EXECUTE 'DROP TABLE IF EXISTS ' || quote_ident(r.tablename) || ' CASCADE';
                END LOOP;
            END $$;
            DO $$ DECLARE
                r RECORD;
            BEGIN
                FOR r IN (SELECT typname FROM pg_type WHERE typtype = 'e' AND typnamespace = (SELECT oid FROM pg_namespace WHERE nspname = current_schema())) LOOP
                    EXECUTE 'DROP TYPE IF EXISTS ' || quote_ident(r.typname) || ' CASCADE';
                END LOOP;
            END $$;",
        )
        .await {
            Ok(rows) => rows,
            Err(e) => {
                try_error!(
                    ctx,
                    "error rolling back test migrations: {}",
                    e.to_string()
                );
                process::exit(1);
            }
        };
}
