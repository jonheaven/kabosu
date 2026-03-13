use dogecoin::types::OrdinalInscriptionRevealData;
use doginals::{koinu::Koinu, rarity::Rarity};
use postgres::{types::PgNumericU64, FromPgRow};
use tokio_postgres::Row;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbKoinu {
    pub ordinal_number: PgNumericU64,
    pub rarity: String,
    pub coinbase_height: PgNumericU64,
}

impl DbKoinu {
    pub fn from_reveal(reveal: &OrdinalInscriptionRevealData) -> Self {
        let rarity = Rarity::from(Koinu(reveal.ordinal_number));
        DbKoinu {
            ordinal_number: PgNumericU64(reveal.ordinal_number),
            rarity: rarity.to_string(),
            coinbase_height: PgNumericU64(reveal.ordinal_block_height),
        }
    }
}

impl FromPgRow for DbKoinu {
    fn from_pg_row(row: &Row) -> Self {
        DbKoinu {
            ordinal_number: row.get("ordinal_number"),
            rarity: row.get("rarity"),
            coinbase_height: row.get("coinbase_height"),
        }
    }
}
