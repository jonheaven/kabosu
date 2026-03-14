use doginals_parser::DuneId;
use bitcoin::Txid;

#[derive(Debug, Clone)]
pub struct InputDuneBalance {
    /// Previous owner of this balance. If this is `None`, it means the balance was just minted or premined.
    pub address: Option<String>,
    /// How much balance was input to this transaction.
    pub balance: u128,
    /// Unique identifier for the dune.
    pub dune_id: DuneId,
    /// Transaction ID for the input.
    pub txid: Txid,
    /// Output index (vout).
    pub vout: u32,
    /// Block height where the input was indexed.
    pub block_height: u32,
    /// Timestamp of the block.
    pub timestamp: u64,
}

#[cfg(test)]
impl InputDuneBalance {
    pub fn dummy() -> Self {
        InputDuneBalance {
            dune_id: DuneId::default(),
            balance: 1000,
            txid: Txid::default(),
            vout: 0,
            address: Some("bc1p8zxlhgdsq6dmkzk4ammzcx55c3hfrg69ftx0gzlnfwq0wh38prds0nzqwf".to_string()),
            block_height: 0,
            timestamp: 0,
        }
    }

    pub fn balance(&mut self, balance: u128) -> &mut Self {
        self.balance = balance;
        self
    }

    pub fn address(&mut self, address: Option<String>) -> &mut Self {
        self.address = address;
        self
    }
}
