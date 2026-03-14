use dogecoin::types::DoginalInscriptionRevealData;
use doginals::{koinu::Koinu, rarity::Rarity};
use postgres::{types::PgNumericU64, FromPgRow};
use tokio_postgres::Row;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbKoinu {
    pub doginal_number: PgNumericU64,
    pub rarity: String,
    pub coinbase_height: PgNumericU64,
}

impl DbKoinu {
    pub fn from_reveal(reveal: &DoginalInscriptionRevealData) -> Self {
        let rarity = Rarity::from(Koinu(reveal.doginal_number));
        DbKoinu {
            doginal_number: PgNumericU64(reveal.doginal_number),
            rarity: rarity.to_string(),
            coinbase_height: PgNumericU64(reveal.doginal_block_height),
        }
    }
}

impl FromPgRow for DbKoinu {
    fn from_pg_row(row: &Row) -> Self {
        DbKoinu {
            doginal_number: row.get("doginal_number"),
            rarity: row.get("rarity"),
            coinbase_height: row.get("coinbase_height"),
        }
    }
}
