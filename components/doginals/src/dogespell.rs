use super::*;

#[derive(Copy, Clone, Debug, PartialEq, DeserializeFromStr, SerializeDisplay)]
pub enum Dogespell {
    Coin = 0,
    Cursed = 1,
    Epic = 2,
    Legendary = 3,
    Lost = 4,
    Nineball = 5,
    Rare = 6,
    Reinscription = 7,
    Unbound = 8,
    Uncommon = 9,
    Vindicated = 10,
    Mythic = 11,
    Burned = 12,
    Palindrome = 13,
}

impl Dogespell {
    pub const ALL: [Self; 14] = [
        Self::Coin,
        Self::Uncommon,
        Self::Rare,
        Self::Epic,
        Self::Legendary,
        Self::Mythic,
        Self::Nineball,
        Self::Palindrome,
        Self::Reinscription,
        Self::Cursed,
        Self::Unbound,
        Self::Lost,
        Self::Vindicated,
        Self::Burned,
    ];

    pub fn flag(self) -> u16 {
        1 << self as u16
    }

    pub fn set(self, dogespells: &mut u16) {
        *dogespells |= self.flag();
    }

    pub fn is_set(self, dogespells: u16) -> bool {
        dogespells & self.flag() != 0
    }

    pub fn unset(self, dogespells: u16) -> u16 {
        dogespells & !self.flag()
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Burned => "🔥",
            Self::Coin => "🪙",
            Self::Cursed => "👹",
            Self::Epic => "🪻",
            Self::Legendary => "🌝",
            Self::Lost => "🤔",
            Self::Mythic => "🎃",
            Self::Nineball => "\u{39}\u{fe0f}\u{20e3}",
            Self::Palindrome => "🦋",
            Self::Rare => "🧿",
            Self::Reinscription => "♻️",
            Self::Unbound => "🔓",
            Self::Uncommon => "🌱",
            Self::Vindicated => "\u{2764}\u{fe0f}\u{200d}\u{1f525}",
        }
    }

    pub fn dogespells(dogespells: u16) -> Vec<Dogespell> {
        Self::ALL
            .into_iter()
            .filter(|dogespell| dogespell.is_set(dogespells))
            .collect()
    }
}

impl Display for Dogespell {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Burned => "burned",
                Self::Coin => "coin",
                Self::Cursed => "cursed",
                Self::Epic => "epic",
                Self::Legendary => "legendary",
                Self::Lost => "lost",
                Self::Mythic => "mythic",
                Self::Nineball => "nineball",
                Self::Palindrome => "palindrome",
                Self::Rare => "rare",
                Self::Reinscription => "reinscription",
                Self::Unbound => "unbound",
                Self::Uncommon => "uncommon",
                Self::Vindicated => "vindicated",
            }
        )
    }
}

impl FromStr for Dogespell {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "burned" => Self::Burned,
            "coin" => Self::Coin,
            "cursed" => Self::Cursed,
            "epic" => Self::Epic,
            "legendary" => Self::Legendary,
            "lost" => Self::Lost,
            "mythic" => Self::Mythic,
            "nineball" => Self::Nineball,
            "palindrome" => Self::Palindrome,
            "rare" => Self::Rare,
            "reinscription" => Self::Reinscription,
            "unbound" => Self::Unbound,
            "uncommon" => Self::Uncommon,
            "vindicated" => Self::Vindicated,
            _ => return Err(format!("invalid dogespell `{s}`")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag() {
        assert_eq!(Dogespell::Coin.flag(), 0b1);
        assert_eq!(Dogespell::Cursed.flag(), 0b10);
    }

    #[test]
    fn set() {
        let mut flags = 0;
        assert!(!Dogespell::Coin.is_set(flags));
        Dogespell::Coin.set(&mut flags);
        assert!(Dogespell::Coin.is_set(flags));
    }

    #[test]
    fn unset() {
        let mut flags = 0;
        Dogespell::Coin.set(&mut flags);
        assert!(Dogespell::Coin.is_set(flags));
        let flags = Dogespell::Coin.unset(flags);
        assert!(!Dogespell::Coin.is_set(flags));
    }

    #[test]
    fn from_str() {
        for dogespell in Dogespell::ALL {
            assert_eq!(
                dogespell.to_string().parse::<Dogespell>().unwrap(),
                dogespell
            );
        }
    }
}
