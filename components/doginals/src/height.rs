use super::*;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Display, FromStr)]
pub struct Height(pub u32);

impl Height {
    pub fn n(self) -> u32 {
        self.0
    }

    pub fn subsidy(self) -> u64 {
        Epoch::from(self).subsidy()
    }

    pub fn starting_sat(self) -> Koinu {
        let epoch = Epoch::from(self);
        let epoch_starting_sat = epoch.starting_sat();
        let epoch_starting_height = epoch.starting_height();
        epoch_starting_sat + u64::from(self.n() - epoch_starting_height.n()) * epoch.subsidy()
    }

    #[allow(clippy::modulo_one)]
    pub fn period_offset(self) -> u32 {
        self.0 % DIFFCHANGE_INTERVAL
    }
}

impl Add<u32> for Height {
    type Output = Self;

    fn add(self, other: u32) -> Height {
        Self(self.0 + other)
    }
}

impl Sub<u32> for Height {
    type Output = Self;

    fn sub(self, other: u32) -> Height {
        Self(self.0 - other)
    }
}

impl PartialEq<u32> for Height {
    fn eq(&self, other: &u32) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n() {
        assert_eq!(Height(0).n(), 0);
        assert_eq!(Height(1).n(), 1);
    }

    #[test]
    fn add() {
        assert_eq!(Height(0) + 1, 1);
        assert_eq!(Height(1) + 100, 101);
    }

    #[test]
    fn sub() {
        assert_eq!(Height(1) - 1, 0);
        assert_eq!(Height(100) - 50, 50);
    }

    #[test]
    fn eq() {
        assert_eq!(Height(0), 0);
        assert_eq!(Height(100), 100);
    }

    #[test]
    fn from_str() {
        assert_eq!("0".parse::<Height>().unwrap(), 0);
        assert!("foo".parse::<Height>().is_err());
    }

    #[test]
    fn subsidy() {
        // Wonky era: rewards are loaded from subsidies.json — just verify non-zero.
        assert!(Height(0).subsidy() > 0);
        assert!(Height(1).subsidy() > 0);
        // Permanent floor: 10,000 DOGE per block forever from block 600,000.
        assert_eq!(Height(600_000).subsidy(), 10_000 * COIN_VALUE);
        assert_eq!(Height(1_000_000).subsidy(), 10_000 * COIN_VALUE);
    }

    #[test]
    fn starting_sat() {
        // No koinu exist before the genesis block.
        assert_eq!(Height(0).starting_sat(), Koinu(0));
        // Block 1's first koinu equals the genesis block's subsidy.
        assert_eq!(Height(1).starting_sat(), Koinu(Height(0).subsidy()));
        // In the permanent-floor era every block advances by exactly 10,000 DOGE.
        assert_eq!(
            Height(600_001).starting_sat(),
            Height(600_000).starting_sat() + 10_000 * COIN_VALUE,
        );
    }

    #[test]
    fn period_offset() {
        // DIFFCHANGE_INTERVAL = 1 for Dogecoin (AuxPoW retargets every block).
        assert_eq!(Height(0).period_offset(), 0);
        assert_eq!(Height(1).period_offset(), 0);
        assert_eq!(Height(1000).period_offset(), 0);
    }
}
