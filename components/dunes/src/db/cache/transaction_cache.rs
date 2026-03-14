use std::collections::{HashMap, VecDeque};
use dogecoin::{try_debug, try_warn, utils::Context};
use crate::db::DbLedgerEntry;
use crate::db::models::db_ledger_operation::DbLedgerOperation;
use crate::db::models::db_dune::DbDune;
use crate::db::cache::input_dune_balance::InputDuneBalance;
use crate::db::cache::transaction_location::TransactionLocation;
use crate::db::cache::utils::{is_dune_mintable, move_dune_balance_to_output, new_sequential_ledger_entry};
use doginals_parser::{Etching, Dune, Edict};
use bitcoin::ScriptBuf;
use doginals_parser::DuneId;


/// Holds cached data relevant to a single transaction during indexing.
pub struct TransactionCache {
    pub location: TransactionLocation,
    /// Sequential index of the ledger entry we're inserting next for this transaction. Will be increased with each generated
    /// entry.
    next_event_index: u32,
    /// Dune etched during this transaction, if any.
    pub etching: Option<DbDune>,
    /// The output where all unallocated dunes will be transferred to. Set to the first eligible output by default but can be
    /// overridden by a Dunestone.
    pub output_pointer: Option<u32>,
    /// Holds input dunes for the current transaction (input to this tx, premined or minted). Balances in the vector are in the
    /// order in which they were input to this transaction.
    pub input_dunes: HashMap<DuneId, VecDeque<InputDuneBalance>>,
    /// Non-OP_RETURN outputs in this transaction
    eligible_outputs: HashMap<u32, ScriptBuf>,
    /// Total outputs contained in this transaction, including non-eligible outputs.
    total_outputs: u32,
}

impl TransactionCache {
    pub fn new(
        location: TransactionLocation,
        input_dunes: HashMap<DuneId, VecDeque<InputDuneBalance>>,
        eligible_outputs: HashMap<u32, ScriptBuf>,
        first_eligible_output: Option<u32>,
        total_outputs: u32,
    ) -> Self {
        TransactionCache {
            location,
            next_event_index: 0,
            etching: None,
            output_pointer: first_eligible_output,
            input_dunes,
            eligible_outputs,
            total_outputs,
        }
    }

    #[cfg(test)]
    pub fn empty(location: TransactionLocation) -> Self {
        TransactionCache {
            location,
            next_event_index: 0,
            etching: None,
            output_pointer: None,
            input_dunes: maplit::hashmap! {},
            eligible_outputs: maplit::hashmap! {},
            total_outputs: 0,
        }
    }

    /// Burns the dune balances input to this transaction.
    pub fn apply_cenotaph_input_burn(&mut self, _cenotaph: &doginals_parser::Cenotaph) -> Vec<DbLedgerEntry> {
        let mut results = vec![];
        for (dune_id, unallocated) in self.input_dunes.iter() {
            for balance in unallocated {
                results.push(new_sequential_ledger_entry(
                    &self.location,
                    Some(balance.balance),
                    *dune_id,
                    None,
                    balance.address.as_deref(),
                    None,
                    DbLedgerOperation::Burn,
                    &mut self.next_event_index,
                ));
            }
        }
        self.input_dunes.clear();
        results
    }

    /// Moves remaining input dunes to the correct output depending on dunestone configuration. Must be called once the processing
    /// for a transaction is complete.
    pub fn allocate_remaining_balances(&mut self, ctx: &Context) -> Vec<DbLedgerEntry> {
        let mut results = vec![];
        for (dune_id, unallocated) in self.input_dunes.iter_mut() {
            #[cfg(not(feature = "release"))]
            for input in unallocated.iter() {
                try_debug!(
                    ctx,
                    "Assign unallocated {dune_id} to pointer {output_pointer:?} {address:?} ({balance}) {location}",
                    output_pointer = self.output_pointer,
                    address = &input.address,
                    balance = input.balance,
                    location = self.location.to_string()
                );
            }
            results.extend(move_dune_balance_to_output(
                &self.location,
                self.output_pointer,
                dune_id,
                unallocated,
                &self.eligible_outputs,
                0, // All of it
                &mut self.next_event_index,
                ctx,
            ));
        }
        self.input_dunes.clear();
        results
    }

    pub fn apply_etching(
        &mut self,
        etching: &Etching,
        number: u32,
    ) -> (DuneId, DbDune, DbLedgerEntry) {
        let dune_id = self.location.dune_id();
        let db_dune = DbDune::from_etching(etching, number, &self.location);
        self.etching = Some(db_dune.clone());
        // Move pre-mined balance to input dunes.
        if let Some(premine) = etching.premine {
            self.add_input_dunes(
                &dune_id,
                InputDuneBalance {
                    dune_id: dune_id.clone(),
                    balance: premine,
                    txid: self.location.tx_id.clone(),
                    vout: 0,
                    address: None,
                    block_height: self.location.block_height,
                    timestamp: self.location.timestamp,
                },
            );
        }
        let entry = new_sequential_ledger_entry(
            &self.location,
            None,
            dune_id,
            None,
            None,
            None,
            DbLedgerOperation::Etching,
            &mut self.next_event_index,
        );
        (dune_id, db_dune, entry)
    }

    pub fn apply_cenotaph_etching(
        &mut self,
        dune: &Dune,
        number: u32,
    ) -> (DuneId, DbDune, DbLedgerEntry) {
        let dune_id = self.location.dune_id();
        // If the dunestone that produced the cenotaph contained an etching, the etched dune has supply zero and is unmintable.
        let db_dune = DbDune::from_cenotaph_etching(dune, number, &self.location);
        self.etching = Some(db_dune.clone());
        let entry = new_sequential_ledger_entry(
            &self.location,
            None,
            dune_id,
            None,
            None,
            None,
            DbLedgerOperation::Etching,
            &mut self.next_event_index,
        );
        (dune_id, db_dune, entry)
    }

    pub fn apply_mint(
        &mut self,
        dune_id: &DuneId,
        total_mints: u128,
        db_dune: &DbDune,
        ctx: &Context,
    ) -> Option<DbLedgerEntry> {
        if !is_dune_mintable(db_dune, total_mints, &self.location) {
            try_debug!(
                ctx,
                "Invalid mint {dune_id} {location}",
                location = self.location.to_string()
            );
            return None;
        }
        let terms_amount = db_dune.terms_amount.unwrap();
        try_debug!(
            ctx,
            "MINT {dune_id} ({spaced_name}) {amount} {location}",
            spaced_name = &db_dune.spaced_name,
            amount = terms_amount.0,
            location = self.location.to_string()
        );
        self.add_input_dunes(
            dune_id,
            InputDuneBalance {
                dune_id: dune_id.clone(),
                balance: terms_amount.0,
                txid: self.location.tx_id.clone(),
                vout: 0,
                address: None,
                block_height: self.location.block_height,
                timestamp: self.location.timestamp,
            },
        );
        Some(new_sequential_ledger_entry(
            &self.location,
            Some(terms_amount.0),
            *dune_id,
            None,
            None,
            None,
            DbLedgerOperation::Mint,
            &mut self.next_event_index,
        ))
    }

    pub fn apply_cenotaph_mint(
        &mut self,
        dune_id: &DuneId,
        total_mints: u128,
        db_dune: &DbDune,
        ctx: &Context,
    ) -> Option<DbLedgerEntry> {
        if !is_dune_mintable(db_dune, total_mints, &self.location) {
            try_debug!(
                ctx,
                "Invalid mint {dune_id} {location}",
                location = self.location.to_string()
            );
            return None;
        }
        let terms_amount = db_dune.terms_amount.unwrap();
        try_debug!(
            ctx,
            "CENOTAPH MINT {spaced_name} {amount} {location}",
            spaced_name = &db_dune.spaced_name,
            amount = terms_amount.0,
            location = self.location.to_string()
        );
        // This entry does not go in the input dunes, it gets burned immediately.
        Some(new_sequential_ledger_entry(
            &self.location,
            Some(terms_amount.0),
            *dune_id,
            None,
            None,
            None,
            DbLedgerOperation::Burn,
            &mut self.next_event_index,
        ))
    }

    pub fn apply_edict(&mut self, edict: &Edict, ctx: &Context) -> Vec<DbLedgerEntry> {
        // Find this dune.
        let dune_id = if edict.id.block == 0 && edict.id.tx == 0 {
            let Some(etching) = self.etching.as_ref() else {
                try_warn!(
                    ctx,
                    "Attempted edict for nonexistent dune 0:0 {location}",
                    location = self.location.to_string()
                );
                return vec![];
            };
            etching.dune_id()
        } else {
            edict.id
        };
        // Take all the available inputs for the dune we're trying to move.
        let Some(available_inputs) = self.input_dunes.get_mut(&dune_id) else {
            try_debug!(
                ctx,
                "No unallocated dunes {id} remain for edict {location}",
                id = edict.id.to_string(),
                location = self.location.to_string()
            );
            return vec![];
        };
        // Calculate the maximum unallocated balance we can move.
        let unallocated = available_inputs
            .iter()
            .map(|b| b.balance)
            .reduce(|acc, e| acc + e)
            .unwrap_or(0);
        // Perform movements.
        let mut results = vec![];
        if self.eligible_outputs.is_empty() {
            // No eligible outputs means burn.
            try_debug!(
                ctx,
                "No eligible outputs for edict on dune {id} {location}",
                id = edict.id.to_string(),
                location = self.location.to_string()
            );
            results.extend(move_dune_balance_to_output(
                &self.location,
                None, // This will force a burn.
                &dune_id,
                available_inputs,
                &self.eligible_outputs,
                edict.amount,
                &mut self.next_event_index,
                ctx,
            ));
        } else {
            match edict.output {
                // An edict with output equal to the number of transaction outputs allocates `amount` dunes to each non-OP_RETURN
                // output in order.
                output if output == self.total_outputs => {
                    let mut output_keys: Vec<u32> = self.eligible_outputs.keys().cloned().collect();
                    output_keys.sort();
                    if edict.amount == 0 {
                        // Divide equally. If the number of unallocated dunes is not divisible by the number of non-OP_RETURN outputs,
                        // 1 additional dune is assigned to the first R non-OP_RETURN outputs, where R is the remainder after dividing
                        // the balance of unallocated units of dune id by the number of non-OP_RETURN outputs.
                        let len = self.eligible_outputs.len() as u128;
                        let per_output = unallocated / len;
                        let mut remainder = unallocated % len;
                        for output in output_keys {
                            let mut extra = 0;
                            if remainder > 0 {
                                extra = 1;
                                remainder -= 1;
                            }
                            results.extend(move_dune_balance_to_output(
                                &self.location,
                                Some(output),
                                &dune_id,
                                available_inputs,
                                &self.eligible_outputs,
                                per_output + extra,
                                &mut self.next_event_index,
                                ctx,
                            ));
                        }
                    } else {
                        // Give `amount` to all outputs or until unallocated runs out.
                        for output in output_keys {
                            let amount = edict.amount.min(unallocated);
                            results.extend(move_dune_balance_to_output(
                                &self.location,
                                Some(output),
                                &dune_id,
                                available_inputs,
                                &self.eligible_outputs,
                                amount,
                                &mut self.next_event_index,
                                ctx,
                            ));
                        }
                    }
                }
                // Send balance to the output specified by the edict.
                output if output < self.total_outputs => {
                    let mut amount = edict.amount;
                    if edict.amount == 0 {
                        amount = unallocated;
                    }
                    results.extend(move_dune_balance_to_output(
                        &self.location,
                        Some(edict.output),
                        &dune_id,
                        available_inputs,
                        &self.eligible_outputs,
                        amount,
                        &mut self.next_event_index,
                        ctx,
                    ));
                }
                _ => {
                    try_debug!(
                        ctx,
                        "Edict for {id} attempted move to nonexistent output {output}, amount will be burnt {location}",
                        id = edict.id.to_string(),
                        output = edict.output,
                        location = self.location.to_string()
                    );
                    results.extend(move_dune_balance_to_output(
                        &self.location,
                        None, // Burn.
                        &dune_id,
                        available_inputs,
                        &self.eligible_outputs,
                        edict.amount,
                        &mut self.next_event_index,
                        ctx,
                    ));
                }
            }
        }
        results
    }

    fn add_input_dunes(&mut self, dune_id: &DuneId, entry: InputDuneBalance) {
        if let Some(balance) = self.input_dunes.get_mut(dune_id) {
            balance.push_back(entry);
        } else {
            let mut vec = VecDeque::new();
            vec.push_back(entry);
            self.input_dunes.insert(*dune_id, vec);
        }
    }

}
// End of impl TransactionCache

#[cfg(test)]
mod test {
    use std::collections::VecDeque;
    use bitcoin::ScriptBuf;
    use dogecoin::utils::Context;
    use doginals_parser::{Dune, Edict, Etching, Terms};
    use maplit::hashmap;

    use super::TransactionCache;
    use crate::db::{
        cache::{
            input_dune_balance::InputDuneBalance, transaction_location::TransactionLocation,
            utils::is_dune_mintable,
        },
        models::{db_ledger_operation::DbLedgerOperation, db_dune::DbDune},
    };

    #[test]
    fn etches_dune() {
        let location = TransactionLocation::dummy();
        let mut cache = TransactionCache::empty(location.clone());
        let etching = Etching {
            divisibility: Some(2),
            premine: Some(1000),
            dune: Some(Dune::reserved(location.block_height, location.tx_index)),
            spacers: None,
            symbol: Some('x'),
            terms: Some(Terms {
                amount: Some(1000),
                cap: None,
                height: (None, None),
                offset: (None, None),
            }),
            turbo: true,
        };
        let (dune_id, db_dune, db_ledger_entry) = cache.apply_etching(&etching, 1);

        assert_eq!(dune_id.block, 840000);
        assert_eq!(dune_id.tx, 0);
        assert_eq!(db_dune.id, "840000:0");
        assert_eq!(db_dune.name, "AAAAAAAAAAAAAAAAZOMJMODBYFG");
        assert_eq!(db_dune.number.0, 1);
        assert_eq!(db_ledger_entry.operation, DbLedgerOperation::Etching);
        assert_eq!(db_ledger_entry.dune_id, "840000:0");
    #[test]
    fn test_apply_etching_reserved_dune() {
        let location = TransactionLocation::dummy();
        let mut cache = TransactionCache::empty(location.clone());
        let etching = Etching {
            divisibility: Some(2),
            premine: Some(1000),
            dune: None, // Omitted name → should allocate reserved dune
            symbol: Some('x'),
            terms: Some(Terms {
                amount: Some(1000),
                cap: None,
                height: (None, None),
                offset: (None, None),
            }),
            turbo: true,
        };

        let (_dune_id, db_dune, db_ledger_entry) = cache.apply_etching(&etching, 1);
        // Expected reserved dune string for this location
        let expected_reserved_name =
            Dune::reserved(location.block_height, location.tx_index).to_string();

        assert_eq!(db_dune.name, expected_reserved_name);
        assert_eq!(db_ledger_entry.operation, DbLedgerOperation::Etching);
    }
    }

    #[test]
    // TODO add cenotaph field to DbDune before filling this in
    fn etches_cenotaph_dune() {
        let location = TransactionLocation::dummy();
        let mut cache = TransactionCache::empty(location.clone());

        // Create a cenotaph dune
        let dune = Dune::reserved(location.block_height, location.tx_index);
        let number = 2;

        let (_dune_id, db_dune, db_ledger_entry) = cache.apply_cenotaph_etching(&dune, number);

        // // the etched dune has supply zero and is unmintable.
        assert_eq!(is_dune_mintable(&db_dune, 0, &location), false);
        assert_eq!(db_ledger_entry.amount, None);
        assert_eq!(db_dune.id, "840000:0");
        assert_eq!(db_ledger_entry.operation, DbLedgerOperation::Etching);
        assert_eq!(db_ledger_entry.dune_id, "840000:0");
    }

    #[test]
    fn mints_dune() {
        let location = TransactionLocation::dummy();
        let mut cache = TransactionCache::empty(location.clone());
        let db_dune = &DbDune::factory();
        let dune_id = &db_dune.dune_id();

        let ledger_entry = cache.apply_mint(&dune_id, 0, &db_dune, &Context::empty());

        assert!(ledger_entry.is_some());
        let ledger_entry = ledger_entry.unwrap();
        assert_eq!(ledger_entry.operation, DbLedgerOperation::Mint);
        assert_eq!(ledger_entry.dune_id, dune_id.to_string());
        // ledger entry is minted with the correct amount
        assert_eq!(ledger_entry.amount, Some(db_dune.terms_amount.unwrap()));

        // minted amount is added to the input dunes (`cache.input_dunes`)
        assert!(cache.input_dunes.contains_key(&dune_id));
    }

    #[test]
    fn does_not_mint_fully_minted_dune() {
        let location = TransactionLocation::dummy();
        let mut cache = TransactionCache::empty(location.clone());
        let etching = Etching {
            divisibility: Some(2),
            premine: Some(1000),
            dune: Some(Dune::reserved(location.block_height, location.tx_index)),
            spacers: None,
            symbol: Some('x'),
            terms: Some(Terms {
                amount: Some(1000),
                cap: Some(1000),
                height: (None, None),
                offset: (None, None),
            }),
            turbo: true,
        };
        let (dune_id, db_dune, _db_ledger_entry) = cache.apply_etching(&etching, 1);
        let ledger_entry = cache.apply_mint(&dune_id, 1000, &db_dune, &Context::empty());
        assert!(ledger_entry.is_none());
    }

    #[test]
    fn burns_cenotaph_mint() {
        let location = TransactionLocation::dummy();
        let mut cache = TransactionCache::empty(location.clone());

        let db_dune = DbDune::factory();
        let dune_id = db_dune.dune_id();
        let ledger_entry = cache.apply_cenotaph_mint(&dune_id, 0, &db_dune, &Context::empty());
        assert!(ledger_entry.is_some());
        let ledger_entry = ledger_entry.unwrap();
        assert_eq!(ledger_entry.operation, DbLedgerOperation::Burn);
        assert_eq!(
            ledger_entry.amount.unwrap().0,
            db_dune.terms_amount.unwrap().0
        );
    }

    #[test]
    fn moves_dunes_with_edict() {
        let location = TransactionLocation::dummy();
        let db_dune = &DbDune::factory();
        let dune_id = &db_dune.dune_id();
        let mut balances = VecDeque::new();
        let sender_address =
            "bc1p3v7r3n4hv63z4s7jkhdzxsay9xem98hxul057w2mwur406zhw8xqrpwp9w".to_string();
        let receiver_address =
            "bc1p8zxlhgdsq6dmkzk4ammzcx55c3hfrg69ftx0gzlnfwq0wh38prds0nzqwf".to_string();
        balances.push_back(InputDuneBalance {
            dune_id: dune_id.clone(),
            balance: 1000,
            txid: location.tx_id.clone(),
            vout: 0,
            address: Some(sender_address.clone()),
            block_height: location.block_height,
            timestamp: location.timestamp,
        });
        let input_dunes = hashmap! {
            dune_id.clone() => balances
        };
        let eligible_outputs = hashmap! {0=> ScriptBuf::from_hex("5120388dfba1b0069bbb0ad5eef62c1a94c46e91a3454accf40bf34b80f75e2708db").unwrap()};
        let mut cache = TransactionCache::new(location, input_dunes, eligible_outputs, Some(0), 1);

        let edict = Edict {
            id: dune_id.clone(),
            amount: 1000,
            output: 0,
        };

        let ledger_entry = cache.apply_edict(&edict, &Context::empty());
        assert_eq!(ledger_entry.len(), 2);
        let receive = ledger_entry.first().unwrap();
        assert_eq!(receive.operation, DbLedgerOperation::Receive);
        assert_eq!(receive.address, Some(receiver_address.clone()));
        let send = ledger_entry.last().unwrap();
        assert_eq!(send.operation, DbLedgerOperation::Send);
        assert_eq!(send.address, Some(sender_address.clone()));
        assert_eq!(send.receiver_address, Some(receiver_address.clone()));
    }

    #[test]
    fn allocates_remaining_dunes_to_first_eligible_output() {
        let location = TransactionLocation::dummy();
        let db_dune = &DbDune::factory();
        let dune_id = &db_dune.dune_id();
        let mut balances = VecDeque::new();
        let sender_address =
            "bc1p3v7r3n4hv63z4s7jkhdzxsay9xem98hxul057w2mwur406zhw8xqrpwp9w".to_string();
        let receiver_address =
            "bc1p8zxlhgdsq6dmkzk4ammzcx55c3hfrg69ftx0gzlnfwq0wh38prds0nzqwf".to_string();
        balances.push_back(InputDuneBalance {
            address: Some(sender_address.clone()),
            amount: 1000,
        });
        let input_dunes = hashmap! {
            dune_id.clone() => balances
        };
        let eligible_outputs = hashmap! {0=> ScriptBuf::from_hex("5120388dfba1b0069bbb0ad5eef62c1a94c46e91a3454accf40bf34b80f75e2708db").unwrap()};
        let mut cache = TransactionCache::new(location, input_dunes, eligible_outputs, Some(0), 1);
        let ledger_entry = cache.allocate_remaining_balances(&Context::empty());

        assert_eq!(ledger_entry.len(), 2);
        let receive = ledger_entry.first().unwrap();
        assert_eq!(receive.operation, DbLedgerOperation::Receive);
        assert_eq!(receive.address, Some(receiver_address.clone()));
        let send = ledger_entry.last().unwrap();
        assert_eq!(send.operation, DbLedgerOperation::Send);
        assert_eq!(send.address, Some(sender_address.clone()));
        assert_eq!(send.receiver_address, Some(receiver_address.clone()));
    }

    #[test]
    fn allocates_remaining_dunes_to_dunestone_pointer_output() {
        let location = TransactionLocation::dummy();
        let db_dune = &DbDune::factory();
        let dune_id = &db_dune.dune_id();
        let mut balances = VecDeque::new();
        let sender_address =
            "bc1p3v7r3n4hv63z4s7jkhdzxsay9xem98hxul057w2mwur406zhw8xqrpwp9w".to_string();
        let receiver_address =
            "bc1p8zxlhgdsq6dmkzk4ammzcx55c3hfrg69ftx0gzlnfwq0wh38prds0nzqwf".to_string();
        balances.push_back(InputDuneBalance {
            address: Some(sender_address.clone()),
            amount: 1000,
        });
        let input_dunes = hashmap! {
            dune_id.clone() => balances
        };
        let eligible_outputs = hashmap! {1=> ScriptBuf::from_hex("5120388dfba1b0069bbb0ad5eef62c1a94c46e91a3454accf40bf34b80f75e2708db").unwrap()};
        let mut cache = TransactionCache::new(location, input_dunes, eligible_outputs, Some(0), 2);
        cache.output_pointer = Some(1);
        let ledger_entry = cache.allocate_remaining_balances(&Context::empty());

        assert_eq!(ledger_entry.len(), 2);
        let receive = ledger_entry.first().unwrap();
        assert_eq!(receive.operation, DbLedgerOperation::Receive);
        assert_eq!(receive.address, Some(receiver_address.clone()));
        let send = ledger_entry.last().unwrap();
        assert_eq!(send.operation, DbLedgerOperation::Send);
        assert_eq!(send.address, Some(sender_address.clone()));
        assert_eq!(send.receiver_address, Some(receiver_address.clone()));
    }
}
