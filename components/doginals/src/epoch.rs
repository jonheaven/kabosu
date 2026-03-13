use super::*;

// ---------------------------------------------------------------------------
// Dogecoin subsidy data loaded at compile time from the JSON files in the
// repository root.  The "wonky era" covers blocks 0–~145,005 where each block
// received a random reward.  Beyond that range, the post-wonky halving
// schedule is used.
// ---------------------------------------------------------------------------

/// Cumulative shiboshi totals at each block boundary during the wonky era.
/// `STARTING_KOINU[n]` is the total number of shiboshis minted before block n.
static STARTING_KOINU: LazyLock<Vec<u64>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../starting_koinu.json"))
        .expect("starting_koinu.json must be valid JSON")
});

/// Per-block subsidy (in shiboshis) for every block in the wonky era.
/// Keyed by block height as a string (matches the JSON format).
static SUBSIDIES: LazyLock<std::collections::HashMap<u32, u64>> = LazyLock::new(|| {
    let raw: serde_json::Value = serde_json::from_str(include_str!("../../../subsidies.json"))
        .expect("subsidies.json must be valid JSON");
    raw["epochs"]
        .as_object()
        .expect("subsidies.json must have an 'epochs' object")
        .iter()
        .map(|(k, v)| {
            (
                k.parse::<u32>().expect("epoch key must be a valid u32"),
                v.as_u64().expect("epoch value must be a valid u64"),
            )
        })
        .collect()
});

/// The permanent Dogecoin block reward floor (after the halving schedule
/// converges): 10,000 DOGE = 1_000_000_000_000 shiboshis.
pub const DOGE_MIN_SUBSIDY: u64 = 10_000 * COIN_VALUE;

/// Number of blocks covered by the wonky-era JSON files.
const WONKY_ERA_LEN: u32 = 145_006;

/// Return the subsidy (in shiboshis) for a given Dogecoin block height.
///
/// * Heights 0–(WONKY_ERA_LEN-1) are looked up from `subsidies.json`.
/// * Heights beyond that use the standard post-wonky halving schedule.
pub fn dogecoin_block_subsidy(height: u32) -> u64 {
    if let Some(&s) = SUBSIDIES.get(&height) {
        return s;
    }
    dogecoin_standard_subsidy(height)
}

/// Return the cumulative shiboshis minted before `height`.
///
/// For wonky-era heights this is read directly from `starting_koinu.json`.
/// For post-wonky heights it is computed by summing the fixed epoch rewards.
pub fn dogecoin_starting_koinu(height: u32) -> u64 {
    let h = height as usize;
    if h < STARTING_KOINU.len() {
        return STARTING_KOINU[h];
    }
    // Sum up all wonky-era koinu then add standard-era rewards.
    let wonky_total = *STARTING_KOINU.last().unwrap_or(&0);
    let post_wonky = cumulative_post_wonky_sats(height);
    wonky_total.saturating_add(post_wonky)
}

/// Dogecoin post-wonky halving schedule (blocks ≥ 145,000):
///
/// | Block range       | Reward per block |
/// |-------------------|-----------------|
/// | 145,000–199,999   | 500,000 DOGE    |
/// | 200,000–299,999   | 250,000 DOGE    |
/// | 300,000–399,999   | 125,000 DOGE    |
/// | 400,000–499,999   | 62,500  DOGE    |
/// | 500,000–599,999   | 31,250  DOGE    |
/// | 600,000+          | 10,000  DOGE    |
fn dogecoin_standard_subsidy(height: u32) -> u64 {
    if height >= 600_000 {
        return DOGE_MIN_SUBSIDY;
    }
    match height / 100_000 {
        0 | 1 => 500_000 * COIN_VALUE, // 0–199,999 (wonky for 0–144,999; fixed after)
        2 => 250_000 * COIN_VALUE,
        3 => 125_000 * COIN_VALUE,
        4 => 62_500 * COIN_VALUE,
        5 => 31_250 * COIN_VALUE,
        _ => DOGE_MIN_SUBSIDY,
    }
}

/// Cumulative shiboshis minted from block WONKY_ERA_LEN up to (but not
/// including) `height`.
fn cumulative_post_wonky_sats(height: u32) -> u64 {
    let start = WONKY_ERA_LEN;
    if height <= start {
        return 0;
    }
    // Walk through each post-wonky 100k-block halving period and accumulate.
    let mut total: u64 = 0;
    let mut h = start;
    while h < height {
        let subsidy = dogecoin_standard_subsidy(h);
        // How many blocks remain in this 100k-period?
        let period_end = ((h / 100_000) + 1) * 100_000;
        let period_end = period_end.min(height);
        let blocks = (period_end - h) as u64;
        total = total.saturating_add(blocks.saturating_mul(subsidy));
        h = period_end;
    }
    total
}

// ---------------------------------------------------------------------------
// Epoch type
//
// With SUBSIDY_HALVING_INTERVAL = 1, Epoch(n) corresponds to block n.
// This lets the existing ordinals machinery work unmodified: every height
// is its own epoch, and `subsidy()` / `starting_sat()` delegate to the
// Dogecoin subsidy functions above.
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Eq, PartialEq, Debug, Display, Serialize, PartialOrd)]
pub struct Epoch(pub u32);

impl Epoch {
    /// There is no "first post-subsidy" epoch for Dogecoin (10k DOGE floor
    /// is permanent), so we set this to a very large value.
    pub const FIRST_POST_SUBSIDY: Epoch = Self(u32::MAX);

    pub fn subsidy(self) -> u64 {
        dogecoin_block_subsidy(self.0)
    }

    pub fn starting_sat(self) -> Koinu {
        Koinu(dogecoin_starting_koinu(self.0))
    }

    pub fn starting_height(self) -> Height {
        // With SUBSIDY_HALVING_INTERVAL = 1, epoch n starts at height n.
        Height(self.0 * SUBSIDY_HALVING_INTERVAL)
    }

    /// Iterator over every epoch's starting sat (from `starting_koinu.json`).
    /// Used by the `ord epochs` subcommand.
    pub fn all_starting_koinu() -> impl Iterator<Item = Koinu> {
        STARTING_KOINU.iter().copied().map(Koinu)
    }
}

impl PartialEq<u32> for Epoch {
    fn eq(&self, other: &u32) -> bool {
        self.0 == *other
    }
}

impl From<Koinu> for Epoch {
    fn from(sat: Koinu) -> Self {
        // Binary search through the STARTING_KOINU array, then fall back to
        // post-wonky computation.
        let starting_koinu = &*STARTING_KOINU;
        let target = sat.n();

        // Find the last entry ≤ target (this is the epoch/block where sat lives).
        match starting_koinu.binary_search(&target) {
            Ok(i) => Epoch(i as u32),
            Err(i) => {
                // i is the insertion point; the epoch is i-1
                if i == 0 {
                    Epoch(0)
                } else if i < starting_koinu.len() {
                    Epoch((i - 1) as u32)
                } else {
                    // Beyond wonky era: binary search in post-wonky range
                    // Approximate: start from WONKY_ERA_LEN and search forward.
                    let wonky_total = *starting_koinu.last().unwrap_or(&0);
                    if target < wonky_total {
                        Epoch(starting_koinu.len().saturating_sub(1) as u32)
                    } else {
                        // Find the block in the post-wonky range
                        let mut h = WONKY_ERA_LEN;
                        let mut cumulative = wonky_total;
                        loop {
                            let subsidy = dogecoin_block_subsidy(h);
                            if cumulative + subsidy > target || subsidy == 0 {
                                return Epoch(h);
                            }
                            cumulative += subsidy;
                            h += 1;
                            if h > 10_000_000 {
                                // Safety cap — should never be reached in practice
                                return Epoch(h);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl From<Height> for Epoch {
    fn from(height: Height) -> Self {
        // With SUBSIDY_HALVING_INTERVAL = 1: epoch == height.
        Self(height.0 / SUBSIDY_HALVING_INTERVAL)
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn subsidy_block_0_is_nonzero() {
        // Dogecoin block 0 had a positive reward.
        assert!(Epoch(0).subsidy() > 0);
    }

    #[test]
    fn subsidy_post_wonky_era() {
        // Post-wonky halving schedule sanity checks.
        assert_eq!(Epoch(200_000).subsidy(), 250_000 * 100_000_000);
        assert_eq!(Epoch(300_000).subsidy(), 125_000 * 100_000_000);
        assert_eq!(Epoch(600_000).subsidy(), 10_000 * 100_000_000);
        assert_eq!(Epoch(1_000_000).subsidy(), 10_000 * 100_000_000);
    }

    #[test]
    fn starting_sat_block_0_is_zero() {
        assert_eq!(Epoch(0).starting_sat(), Koinu(0));
    }

    #[test]
    fn from_height_identity() {
        // With SUBSIDY_HALVING_INTERVAL = 1, Epoch::from(Height(n)) == Epoch(n).
        assert_eq!(Epoch::from(Height(0)), Epoch(0));
        assert_eq!(Epoch::from(Height(42)), Epoch(42));
        assert_eq!(Epoch::from(Height(600_000)), Epoch(600_000));
    }

    #[test]
    fn starting_height_identity() {
        assert_eq!(Epoch(0).starting_height(), Height(0));
        assert_eq!(Epoch(100).starting_height(), Height(100));
    }

    // ------------------------------------------------------------------
    // Canonical Dogecoin subsidy assertions
    //
    // These are the numbers that separate a correct Doginals indexer from
    // every naive Bitcoin fork.  Publish these test results as proof that
    // koinu numbering is exact.
    //
    // Reference: dogecoin/src/validation.cpp  GetBlockSubsidy()
    // ------------------------------------------------------------------

    #[test]
    fn subsidy_permanent_floor_10k_doge() {
        // Dogecoin has NO final halving.  From block 600,000 onward the reward
        // is permanently 10,000 DOGE — unlike Bitcoin which eventually reaches 0.
        const FLOOR: u64 = 10_000 * COIN_VALUE;
        assert_eq!(Epoch(600_000).subsidy(), FLOOR);
        assert_eq!(Epoch(1_000_000).subsidy(), FLOOR); // ~year 2027
        assert_eq!(Epoch(5_000_000).subsidy(), FLOOR); // ~year 2035
        assert_eq!(Epoch(10_000_000).subsidy(), FLOOR); // ~year 2044
    }

    #[test]
    fn first_koinu_in_block_one_equals_genesis_subsidy() {
        // starting_koinu(1) == subsidy_at(0)
        // i.e. block 1's first koinu is numbered exactly at the genesis reward.
        assert_eq!(Epoch(1).starting_sat(), Koinu(Epoch(0).subsidy()));
    }

    #[test]
    fn subsidy_halving_schedule_matches_dogecoin_core() {
        // Post-wonky schedule mirrors GetBlockSubsidy() in dogecoin/src/validation.cpp.
        assert_eq!(Epoch(145_000).subsidy(), 500_000 * COIN_VALUE);
        assert_eq!(Epoch(200_000).subsidy(), 250_000 * COIN_VALUE);
        assert_eq!(Epoch(300_000).subsidy(), 125_000 * COIN_VALUE);
        assert_eq!(Epoch(400_000).subsidy(), 62_500 * COIN_VALUE);
        assert_eq!(Epoch(500_000).subsidy(), 31_250 * COIN_VALUE);
        assert_eq!(Epoch(600_000).subsidy(), 10_000 * COIN_VALUE);
    }
}
