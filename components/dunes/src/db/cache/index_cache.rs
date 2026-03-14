use std::{collections::HashMap, num::NonZeroUsize, str::FromStr};

use bitcoin::{Network, ScriptBuf};
use config::{Config, DogecoinConfig};
use deadpool_postgres::{Pool, Transaction};
use dogecoin::{
    bitcoincore_rpc::Client as BitcoinRpcClient,
    try_debug, try_error, try_warn,
    types::dogecoin::TxIn,
    utils::{dogecoind::dogecoin_get_client, Context},
};
use doginals_parser::{Cenotaph, Dune, DuneId, Dunestone, Edict, Etching, Height};
use lru::LruCache;
use postgres::pg_pool_client;

use super::{
    db_cache::DbCache, transaction_cache::TransactionCache,
    transaction_location::TransactionLocation, utils::move_block_output_cache_to_output_cache,
};
use crate::db::InputDuneBalance;
use crate::db::{
    cache::{
        dune_validation::dune_etching_has_valid_commit, utils::input_dune_balances_from_tx_inputs,
    },
    models::{
        db_balance_change::DbBalanceChange, db_ledger_entry::DbLedgerEntry,
        db_ledger_operation::DbLedgerOperation, db_dune::DbDune, db_supply_change::DbSupplyChange,
    },
    pg_get_max_dune_number, pg_get_dune_by_id, pg_get_dune_total_mints,
};

/// Holds dune data across multiple blocks for faster computations. Processes dune events as they happen during transactions and
/// generates database rows for later insertion.
pub struct IndexCache {
    pub network: Network,
    /// Number to be assigned to the next dune etching.
    next_dune_number: u32,
    /// LRU cache for dunes.
    dune_cache: LruCache<DuneId, DbDune>,
    /// LRU cache for total mints for dunes.
    dune_total_mints_cache: LruCache<DuneId, u128>,
    /// LRU cache for outputs with dune balances.
    output_cache: LruCache<(String, u32), HashMap<DuneId, Vec<InputDuneBalance>>>,
    /// Same as above but only for the current block. We use a `HashMap` instead of an LRU cache to make sure we keep all outputs
    /// in memory while we index this block. Must be cleared every time a new block is processed.
    block_output_cache: HashMap<(String, u32), HashMap<DuneId, Vec<InputDuneBalance>>>,
    /// Holds a single transaction's dune cache. Must be cleared every time a new transaction is processed.
    tx_cache: TransactionCache,
    /// Keeps rows that have not yet been inserted in the DB.
    pub db_cache: DbCache,
    /// Bitcoin RPC client used to validate dune commitments.
    pub dogecoin_client: BitcoinRpcClient,
    /// Bitcoin RPC client configuration.
    dogecoin_client_config: DogecoinConfig,
    /// Current minimum unlocked dune name threshold for explicit names.
    minimum_dune: Dune,
}

impl IndexCache {
    pub async fn new(config: &Config, pg_pool: &Pool, ctx: &Context) -> Self {
        let pg_client = pg_pool_client(pg_pool).await.unwrap();
        let network = config.dogecoin.network;
        let cap = NonZeroUsize::new(config.dunes.as_ref().unwrap().lru_cache_size).unwrap();
        let dogecoin_client = dogecoin_get_client(&config.dogecoin, ctx);
        IndexCache {
            network,
            next_dune_number: pg_get_max_dune_number(&pg_client).await + 1,
            dune_cache: LruCache::new(cap),
            dune_total_mints_cache: LruCache::new(cap),
            output_cache: LruCache::new(cap),
            block_output_cache: HashMap::new(),
            tx_cache: TransactionCache::new(
                TransactionLocation {
                    network,
                    block_hash: "".to_string(),
                    block_height: 1,
                    timestamp: 0,
                    tx_index: 0,
                    tx_id: "".to_string(),
                },
                HashMap::new(),
                HashMap::new(),
                None,
                0,
            ),
            db_cache: DbCache::new(),
            dogecoin_client,
            dogecoin_client_config: config.dogecoin.clone(),
            minimum_dune: Dune(0),
        }
    }

    /// Recreate and replace the internal Bitcoin RPC client
    pub fn reconnect_dogecoin_client(&mut self, ctx: &Context) {
        self.dogecoin_client = dogecoin_get_client(&self.dogecoin_client_config, ctx);
    }

    pub async fn reset_max_dune_number(&mut self, db_tx: &mut Transaction<'_>) {
        self.next_dune_number = pg_get_max_dune_number(db_tx).await + 1;
    }

    /// Creates a fresh transaction index cache.
    #[allow(clippy::too_many_arguments)]
    pub async fn begin_transaction(
        &mut self,
        location: TransactionLocation,
        tx_inputs: &[TxIn],
        eligible_outputs: HashMap<u32, ScriptBuf>,
        first_eligible_output: Option<u32>,
        total_outputs: u32,
        db_tx: &mut Transaction<'_>,
        ctx: &Context,
    ) {
        // Update the dynamic minimum name threshold based on current block height.
        self.minimum_dune =
            Dune::minimum_at_height(self.network, Height(location.block_height as u32));

        let input_dunes = input_dune_balances_from_tx_inputs(
            tx_inputs,
            &self.block_output_cache,
            &mut self.output_cache,
            db_tx,
            ctx,
        )
        .await;
        #[cfg(not(feature = "release"))]
        {
            for (dune_id, balances) in input_dunes.iter() {
                try_debug!(ctx, "INPUT {dune_id} {balances:?} {location}");
            }
            if !input_dunes.is_empty() {
                try_debug!(
                    ctx,
                    "First output: {first_eligible_output:?}, total_outputs: {total_outputs}"
                );
            }
        }
        self.tx_cache = TransactionCache::new(
            location,
            input_dunes,
            eligible_outputs,
            first_eligible_output,
            total_outputs,
        );
    }

    /// Finalizes the current transaction index cache by moving all unallocated balances to the correct output.
    pub fn end_transaction(&mut self, _db_tx: &mut Transaction<'_>, ctx: &Context) {
        let entries = self.tx_cache.allocate_remaining_balances(ctx);
        self.add_ledger_entries_to_db_cache(&entries);
    }

    pub fn end_block(&mut self) {
        move_block_output_cache_to_output_cache(
            &mut self.block_output_cache,
            &mut self.output_cache,
        );
    }

    pub async fn apply_dunestone(
        &mut self,
        dunestone: &Dunestone,
        _db_tx: &mut Transaction<'_>,
        ctx: &Context,
    ) {
        try_debug!(ctx, "{:?} {}", dunestone, self.tx_cache.location);
        if let Some(new_pointer) = dunestone.pointer {
            self.tx_cache.output_pointer = Some(new_pointer);
        }
    }

    pub async fn apply_cenotaph(
        &mut self,
        cenotaph: &Cenotaph,
        _db_tx: &mut Transaction<'_>,
        ctx: &Context,
        cenotaphs_counter: &mut u64,
    ) {
        try_debug!(ctx, "{:?} {}", cenotaph, self.tx_cache.location);
        let entries = self.tx_cache.apply_cenotaph_input_burn(cenotaph);
        self.add_ledger_entries_to_db_cache(&entries);
        *cenotaphs_counter += 1;
    }

    pub async fn apply_etching(
        &mut self,
        etching: &Etching,
        _db_tx: &mut Transaction<'_>,
        ctx: &Context,
        etchings_counter: &mut u64,
        bitcoin_tx: &bitcoin::Transaction,
        inputs_counter: &mut u64,
    ) -> Result<(), String> {
        match etching.dune {
            // Explicitly reserved names are rejected
            Some(dune) if dune.is_reserved() => {
                try_debug!(
                    ctx,
                    "Skipping etching with explicitly reserved dune {}",
                    dune
                );
                return Ok(());
            }
            // Reject explicit names that are below the currently unlocked minimum.
            Some(dune) if dune < self.minimum_dune => {
                try_debug!(
                    ctx,
                    "Skipping etching with name {} below minimum {} at {}",
                    dune,
                    self.minimum_dune,
                    self.tx_cache.location.to_string()
                );
                return Ok(());
            }
            // Explicit non-reserved names require commit validation
            Some(dune) => {
                if !dune_etching_has_valid_commit(
                    &self.dogecoin_client,
                    ctx,
                    bitcoin_tx,
                    &dune,
                    self.tx_cache.location.block_height as u32,
                    inputs_counter,
                )
                .await?
                {
                    try_error!(ctx, "Invalid dune commitment for etching {dune}");
                    return Ok(());
                }
            }
            None => {}
        }

        let (dune_id, db_dune, entry) = self.tx_cache.apply_etching(etching, self.next_dune_number);

        try_debug!(
            ctx,
            "Etching {spaced_name} ({id}) {location}",
            spaced_name = &db_dune.spaced_name,
            id = &db_dune.id,
            location = self.tx_cache.location.to_string()
        );
        self.db_cache.dunes.push(db_dune.clone());
        self.dune_cache.put(dune_id, db_dune);
        self.add_ledger_entries_to_db_cache(&vec![entry]);
        self.next_dune_number += 1;
        *etchings_counter += 1;
        Ok(())
    }

    pub async fn apply_cenotaph_etching(
        &mut self,
        dune: &Dune,
        _db_tx: &mut Transaction<'_>,
        ctx: &Context,
        cenotaph_etchings_counter: &mut u64,
        bitcoin_tx: &bitcoin::Transaction,
        inputs_counter: &mut u64,
    ) -> Result<(), String> {
        // Explicitly reserved names are rejected
        if dune.is_reserved() {
            try_debug!(
                ctx,
                "Skipping cenotaph etching with explicitly reserved dune {}",
                dune
            );
            return Ok(());
        }

        // Reject names that are below the currently unlocked minimum
        if *dune < self.minimum_dune {
            try_debug!(
                ctx,
                "Skipping cenotaph etching with name {} below minimum {} at {}",
                dune,
                self.minimum_dune,
                self.tx_cache.location.to_string()
            );
            return Ok(());
        }

        // Validate commit for cenotaph etchings as well
        if !dune_etching_has_valid_commit(
            &self.dogecoin_client,
            ctx,
            bitcoin_tx,
            dune,
            self.tx_cache.location.block_height as u32,
            inputs_counter,
        )
        .await?
        {
            try_error!(ctx, "Invalid dune commitment for cenotaph etching {dune}");
            return Ok(());
        }

        let (dune_id, db_dune, entry) = self
            .tx_cache
            .apply_cenotaph_etching(dune, self.next_dune_number);
        try_debug!(
            ctx,
            "Etching cenotaph {spaced_name} ({id}) {location}",
            spaced_name = &db_dune.spaced_name,
            id = &db_dune.id,
            location = self.tx_cache.location.to_string()
        );
        self.db_cache.dunes.push(db_dune.clone());
        self.dune_cache.put(dune_id, db_dune);
        self.add_ledger_entries_to_db_cache(&vec![entry]);
        self.next_dune_number += 1;
        *cenotaph_etchings_counter += 1;
        Ok(())
    }

    pub async fn apply_mint(
        &mut self,
        dune_id: &DuneId,
        db_tx: &mut Transaction<'_>,
        ctx: &Context,
        mints_counter: &mut u64,
    ) {
        let Some(db_dune) = self.get_cached_dune_by_dune_id(dune_id, db_tx, ctx).await else {
            try_warn!(
                ctx,
                "Dune {dune_id} not found for mint {location}",
                location = self.tx_cache.location.to_string()
            );
            return;
        };
        let total_mints = self
            .get_cached_dune_total_mints(dune_id, db_tx, ctx)
            .await
            .unwrap_or(0);
        if let Some(ledger_entry) = self
            .tx_cache
            .apply_mint(dune_id, total_mints, &db_dune, ctx)
        {
            self.add_ledger_entries_to_db_cache(&vec![ledger_entry.clone()]);
            if let Some(total) = self.dune_total_mints_cache.get_mut(dune_id) {
                *total += 1;
            } else {
                self.dune_total_mints_cache.put(*dune_id, 1);
            }
            *mints_counter += 1;
        }
    }

    pub async fn apply_cenotaph_mint(
        &mut self,
        dune_id: &DuneId,
        db_tx: &mut Transaction<'_>,
        ctx: &Context,
        cenotaph_mints_counter: &mut u64,
    ) {
        let Some(db_dune) = self.get_cached_dune_by_dune_id(dune_id, db_tx, ctx).await else {
            try_warn!(
                ctx,
                "Dune {dune_id} not found for cenotaph mint {location}",
                location = self.tx_cache.location.to_string()
            );
            return;
        };
        let total_mints = self
            .get_cached_dune_total_mints(dune_id, db_tx, ctx)
            .await
            .unwrap_or(0);
        if let Some(ledger_entry) =
            self.tx_cache
                .apply_cenotaph_mint(dune_id, total_mints, &db_dune, ctx)
        {
            self.add_ledger_entries_to_db_cache(&vec![ledger_entry]);
            if let Some(total) = self.dune_total_mints_cache.get_mut(dune_id) {
                *total += 1;
            } else {
                self.dune_total_mints_cache.put(*dune_id, 1);
            }
            *cenotaph_mints_counter += 1;
        }
    }

    pub async fn apply_edict(
        &mut self,
        edict: &Edict,
        db_tx: &mut Transaction<'_>,
        ctx: &Context,
        edicts_number: &mut u64,
    ) {
        let Some(db_dune) = self.get_cached_dune_by_dune_id(&edict.id, db_tx, ctx).await else {
            try_warn!(
                ctx,
                "Dune {id} not found for edict {location}",
                id = edict.id.to_string(),
                location = self.tx_cache.location.to_string()
            );
            return;
        };
        let entries = self.tx_cache.apply_edict(edict, ctx);
        for entry in entries.iter() {
            try_debug!(
                ctx,
                "Edict {spaced_name} {amount} {location}",
                spaced_name = &db_dune.spaced_name,
                amount = entry.amount.unwrap().0,
                location = self.tx_cache.location.to_string()
            );
        }
        *edicts_number += 1;
        self.add_ledger_entries_to_db_cache(&entries);
    }

    async fn get_cached_dune_by_dune_id(
        &mut self,
        dune_id: &DuneId,
        db_tx: &mut Transaction<'_>,
        ctx: &Context,
    ) -> Option<DbDune> {
        // Id 0:0 is used to mean the dune being etched in this transaction, if any.
        if dune_id.block == 0 && dune_id.tx == 0 {
            return self.tx_cache.etching.clone();
        }
        if let Some(cached_dune) = self.dune_cache.get(dune_id) {
            return Some(cached_dune.clone());
        }
        // Cache miss, look in DB.
        self.db_cache.flush(db_tx, ctx).await;
        let db_dune = pg_get_dune_by_id(dune_id, db_tx, ctx).await?;
        self.dune_cache.put(*dune_id, db_dune.clone());
        Some(db_dune)
    }

    async fn get_cached_dune_total_mints(
        &mut self,
        dune_id: &DuneId,
        db_tx: &mut Transaction<'_>,
        ctx: &Context,
    ) -> Option<u128> {
        let real_dune_id = if dune_id.block == 0 && dune_id.tx == 0 {
            let etching = self.tx_cache.etching.as_ref()?;
            DuneId::from_str(etching.id.as_str()).unwrap()
        } else {
            *dune_id
        };
        if let Some(total) = self.dune_total_mints_cache.get(&real_dune_id) {
            return Some(*total);
        }
        // Cache miss, look in DB.
        self.db_cache.flush(db_tx, ctx).await;
        let total = pg_get_dune_total_mints(dune_id, db_tx, ctx).await?;
        self.dune_total_mints_cache.put(*dune_id, total);
        Some(total)
    }

    /// Take ledger entries returned by the `TransactionCache` and add them to the `DbCache`. Update global balances and counters
    /// as well.
    fn add_ledger_entries_to_db_cache(&mut self, entries: &[DbLedgerEntry]) {
        self.db_cache.ledger_entries.extend(entries.iter().cloned());
        for entry in entries.iter() {
            match entry.operation {
                DbLedgerOperation::Etching => {
                    self.db_cache
                        .supply_changes
                        .entry(entry.dune_id.clone())
                        .and_modify(|i| {
                            i.total_operations += 1;
                        })
                        .or_insert(DbSupplyChange::from_operation(
                            entry.dune_id.clone(),
                            entry.block_height,
                        ));
                }
                DbLedgerOperation::Mint => {
                    self.db_cache
                        .supply_changes
                        .entry(entry.dune_id.clone())
                        .and_modify(|i| {
                            i.minted += entry.amount.unwrap();
                            i.total_mints += 1;
                            i.total_operations += 1;
                        })
                        .or_insert(DbSupplyChange::from_mint(
                            entry.dune_id.clone(),
                            entry.block_height,
                            entry.amount.unwrap(),
                        ));
                }
                DbLedgerOperation::Burn => {
                    self.db_cache
                        .supply_changes
                        .entry(entry.dune_id.clone())
                        .and_modify(|i| {
                            i.burned += entry.amount.unwrap();
                            i.total_burns += 1;
                            i.total_operations += 1;
                        })
                        .or_insert(DbSupplyChange::from_burn(
                            entry.dune_id.clone(),
                            entry.block_height,
                            entry.amount.unwrap(),
                        ));
                }
                DbLedgerOperation::Send => {
                    self.db_cache
                        .supply_changes
                        .entry(entry.dune_id.clone())
                        .and_modify(|i| i.total_operations += 1)
                        .or_insert(DbSupplyChange::from_operation(
                            entry.dune_id.clone(),
                            entry.block_height,
                        ));
                    if let Some(address) = entry.address.clone() {
                        self.db_cache
                            .balance_deductions
                            .entry((entry.dune_id.clone(), address.clone()))
                            .and_modify(|i| i.balance += entry.amount.unwrap())
                            .or_insert(DbBalanceChange::from_operation(
                                entry.dune_id.clone(),
                                entry.block_height,
                                address,
                                entry.amount.unwrap(),
                            ));
                    }
                }
                DbLedgerOperation::Receive => {
                    self.db_cache
                        .supply_changes
                        .entry(entry.dune_id.clone())
                        .and_modify(|i| i.total_operations += 1)
                        .or_insert(DbSupplyChange::from_operation(
                            entry.dune_id.clone(),
                            entry.block_height,
                        ));
                    if let Some(address) = entry.address.clone() {
                        self.db_cache
                            .balance_increases
                            .entry((entry.dune_id.clone(), address.clone()))
                            .and_modify(|i| i.balance += entry.amount.unwrap())
                            .or_insert(DbBalanceChange::from_operation(
                                entry.dune_id.clone(),
                                entry.block_height,
                                address,
                                entry.amount.unwrap(),
                            ));
                        // Add to current block's output cache if it's received balance.
                        let k = (entry.tx_id.clone(), entry.output.unwrap().0);
                        let dune_id = DuneId::from_str(entry.dune_id.as_str()).unwrap();
                        let balance = InputDuneBalance {
                            dune_id: entry.dune_id.clone(),
                            balance: entry.amount.unwrap().0 as u64,
                            txid: entry.tx_id.clone(),
                            vout: entry.output.unwrap().0,
                            address: entry.address.clone().unwrap_or_default(),
                            block_height: entry.block_height.0 as u64,
                            timestamp: entry.timestamp.0 as u64,
                        };
                        let mut default = HashMap::new();
                        default.insert(dune_id, vec![balance.clone()]);
                        self.block_output_cache
                            .entry(k)
                            .and_modify(|i| {
                                i.entry(dune_id)
                                    .and_modify(|v| v.push(balance.clone()))
                                    .or_insert(vec![balance]);
                            })
                            .or_insert(default);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::{absolute::LockTime, transaction::Version, Transaction};
    use config::{Config, PgDatabaseConfig};
    use dogecoin::utils::Context;
    use doginals_parser::Dune;
    use postgres::{pg_begin, pg_pool, pg_pool_client};

    use super::*;

    #[tokio::test]
    async fn apply_etching_skips_with_early_returns() {
        // Minimal IndexCache without hitting DB in constructor
        let ctx = Context::empty();
        let network = bitcoin::Network::Bitcoin;
        let cap = std::num::NonZeroUsize::new(1).unwrap();
        let mut index = IndexCache {
            network,
            next_dune_number: 1,
            dune_cache: lru::LruCache::new(cap),
            dune_total_mints_cache: lru::LruCache::new(cap),
            output_cache: lru::LruCache::new(cap),
            block_output_cache: HashMap::new(),
            tx_cache: TransactionCache::new(
                TransactionLocation {
                    network,
                    block_hash: "".to_string(),
                    block_height: 840_000,
                    timestamp: 0,
                    tx_index: 0,
                    tx_id: "".to_string(),
                },
                HashMap::new(),
                HashMap::new(),
                None,
                0,
            ),
            db_cache: DbCache::new(),
            dogecoin_client: dogecoind::utils::dogecoind::dogecoin_get_client(
                &Config::test_default().dogecoind,
                &ctx,
            ),
            dogecoin_client_config: Config::test_default().dogecoind,
            minimum_dune: Dune::minimum_at_height(network, Height(840_000)),
        };

        let start_n = index.next_dune_number;

        // Deadpool transaction to satisfy signature (won't be used due to early returns)
        let pool = pg_pool(&PgDatabaseConfig {
            dbname: "postgres".to_string(),
            host: "localhost".to_string(),
            port: 5432,
            user: "postgres".to_string(),
            password: Some("postgres".to_string()),
            search_path: None,
            pool_max_size: Some(1),
        })
        .expect("pool");
        let mut client = pg_pool_client(&pool).await.expect("client");
        let mut db_tx = pg_begin(&mut client).await.expect("tx");

        // Create an empty tx
        let tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![],
            output: vec![],
        };
        let mut inputs_counter = 0;

        // Reserved explicit name
        let etching_reserved = Etching {
            dune: Some(Dune::reserved(840_000, 0)),
            ..Default::default()
        };
        let res_reserved = index
            .apply_etching(
                &etching_reserved,
                &mut db_tx,
                &ctx,
                &mut 0u64,
                &tx,
                &mut inputs_counter,
            )
            .await;
        assert!(res_reserved.is_ok());
        assert_eq!(index.next_dune_number, start_n);

        // Lexicographically above max name
        let etching_above_max = Etching {
            dune: Some("DOGDOGDOGDOGDOGDOGDOGDOG".parse().unwrap()),
            ..Default::default()
        };
        let res_above = index
            .apply_etching(
                &etching_above_max,
                &mut db_tx,
                &ctx,
                &mut 0u64,
                &tx,
                &mut inputs_counter,
            )
            .await;
        assert!(res_above.is_ok());
        assert_eq!(index.next_dune_number, start_n);

        // Below minimum should also be skipped; choose a small dune 'A'
        let etching_below_min = Etching {
            dune: Some("A".parse().unwrap()),
            ..Default::default()
        };
        let res_below = index
            .apply_etching(
                &etching_below_min,
                &mut db_tx,
                &ctx,
                &mut 0u64,
                &tx,
                &mut inputs_counter,
            )
            .await;
        assert!(res_below.is_ok());
        assert_eq!(index.next_dune_number, start_n);
    }
}
