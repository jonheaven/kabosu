use dogecoin::types::{
    BlockIdentifier, DoginalInscriptionRevealData, DoginalInscriptionTransferData,
    DoginalInscriptionTransferDestination, TransactionIdentifier,
};
use postgres::{
    types::{PgBigIntU32, PgNumericU64},
    FromPgRow,
};
use tokio_postgres::Row;

use crate::core::protocol::koinu_tracking::parse_output_and_offset_from_koinupoint;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbCurrentLocation {
    pub doginal_number: PgNumericU64,
    pub block_height: PgNumericU64,
    pub tx_id: String,
    pub tx_index: PgBigIntU32,
    pub address: Option<String>,
    pub output: String,
    pub offset: Option<PgNumericU64>,
}

impl DbCurrentLocation {
    pub fn from_reveal(
        reveal: &DoginalInscriptionRevealData,
        block_identifier: &BlockIdentifier,
        tx_identifier: &TransactionIdentifier,
        tx_index: usize,
    ) -> Self {
        let (output, offset) =
            parse_output_and_offset_from_koinupoint(&reveal.koinupoint_post_inscription).unwrap();
        DbCurrentLocation {
            doginal_number: PgNumericU64(reveal.doginal_number),
            block_height: PgNumericU64(block_identifier.index),
            tx_id: tx_identifier.hash[2..].to_string(),
            tx_index: PgBigIntU32(tx_index as u32),
            address: reveal.inscriber_address.clone(),
            output,
            offset: offset.map(PgNumericU64),
        }
    }

    pub fn from_transfer(
        transfer: &DoginalInscriptionTransferData,
        block_identifier: &BlockIdentifier,
        tx_identifier: &TransactionIdentifier,
        tx_index: usize,
    ) -> Self {
        let (output, offset) =
            parse_output_and_offset_from_koinupoint(&transfer.koinupoint_post_transfer).unwrap();
        DbCurrentLocation {
            doginal_number: PgNumericU64(transfer.doginal_number),
            block_height: PgNumericU64(block_identifier.index),
            tx_id: tx_identifier.hash[2..].to_string(),
            tx_index: PgBigIntU32(tx_index as u32),
            address: match &transfer.destination {
                DoginalInscriptionTransferDestination::Transferred(address) => {
                    Some(address.clone())
                }
                DoginalInscriptionTransferDestination::SpentInFees => None,
                DoginalInscriptionTransferDestination::Burnt(_) => None,
            },
            output,
            offset: offset.map(PgNumericU64),
        }
    }
}

impl FromPgRow for DbCurrentLocation {
    fn from_pg_row(row: &Row) -> Self {
        DbCurrentLocation {
            doginal_number: row.get("doginal_number"),
            block_height: row.get("block_height"),
            tx_id: row.get("tx_id"),
            tx_index: row.get("tx_index"),
            address: row.get("address"),
            output: row.get("output"),
            offset: row.get("offset"),
        }
    }
}
