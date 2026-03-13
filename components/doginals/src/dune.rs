use super::*;

#[derive(
    Default,
    Debug,
    PartialEq,
    Copy,
    Clone,
    PartialOrd,
    Ord,
    Eq,
    DeserializeFromStr,
    SerializeDisplay,
)]
pub struct Dune(pub u128);

impl Dune {
    pub const RESERVED: u128 = 6402364363415443603228541259936211926;

    const UNLOCKED: usize = 12;

    // Guard against zero (Dogecoin's SUBSIDY_HALVING_INTERVAL=1, 1/12=0 in
    // integer division which would cause a divide-by-zero; dunes are a
    // Bitcoin-only concept and this path won't be reached on Dogecoin).
    const UNLOCK_INTERVAL: u32 = if SUBSIDY_HALVING_INTERVAL >= 12 {
        SUBSIDY_HALVING_INTERVAL / 12
    } else {
        1
    };

    const STEPS: &'static [u128] = &[
        0,
        26,
        702,
        18278,
        475254,
        12356630,
        321272406,
        8353082582,
        217180147158,
        5646683826134,
        146813779479510,
        3817158266467286,
        99246114928149462,
        2580398988131886038,
        67090373691429037014,
        1744349715977154962390,
        45353092615406029022166,
        1179180408000556754576342,
        30658690608014475618984918,
        797125955808376366093607894,
        20725274851017785518433805270,
        538857146126462423479278937046,
        14010285799288023010461252363222,
        364267430781488598271992561443798,
        9470953200318703555071806597538774,
        246244783208286292431866971536008150,
        6402364363415443603228541259936211926,
        166461473448801533683942072758341510102,
    ];

    pub fn n(self) -> u128 {
        self.0
    }

    pub fn first_dune_height(network: Network) -> u32 {
        SUBSIDY_HALVING_INTERVAL
            * match network {
                Network::Bitcoin => 4,
                Network::Regtest => 0,
                Network::Signet => 0,
                Network::Testnet => 12,
                _ => 0,
            }
    }

    pub fn minimum_at_height(network: Network, height: Height) -> Self {
        let offset = height.0.saturating_add(1);

        let start = Self::first_dune_height(network);

        let end = start + SUBSIDY_HALVING_INTERVAL;

        if offset < start {
            return Dune(Self::STEPS[Self::UNLOCKED]);
        }

        if offset >= end {
            return Dune(0);
        }

        let progress = offset.saturating_sub(start);

        let length = u32::try_from(Self::UNLOCKED)
            .unwrap()
            .saturating_sub(progress / Self::UNLOCK_INTERVAL);

        let end = Self::STEPS[usize::try_from(length - 1).unwrap()];

        let start = Self::STEPS[usize::try_from(length).unwrap()];

        #[allow(clippy::modulo_one)]
        let remainder = u128::from(progress % Self::UNLOCK_INTERVAL);

        Dune(start - ((start - end) * remainder / u128::from(Self::UNLOCK_INTERVAL)))
    }

    pub fn unlock_height(self, network: Network) -> Option<Height> {
        if self.is_reserved() {
            return None;
        }

        if self.0 >= Self::STEPS[Self::UNLOCKED] {
            return Some(Height(0));
        }

        let i = Self::STEPS.iter().position(|&step| self.0 < step).unwrap();

        let start = Self::STEPS[i];
        let end = i.checked_sub(1).map(|i| Self::STEPS[i]).unwrap_or_default();

        let interval = start - end;
        let progress = start - self.0;

        let height = Self::first_dune_height(network)
            + u32::try_from(Self::UNLOCKED - i).unwrap() * Self::UNLOCK_INTERVAL
            + u32::try_from((progress * u128::from(Self::UNLOCK_INTERVAL) - 1) / interval).unwrap();

        Some(Height(height))
    }

    pub fn is_reserved(self) -> bool {
        self.0 >= Self::RESERVED
    }

    pub fn reserved(block: u64, tx: u32) -> Self {
        Self(
            Self::RESERVED
                .checked_add((u128::from(block) << 32) | u128::from(tx))
                .unwrap(),
        )
    }

    pub fn commitment(self) -> Vec<u8> {
        let bytes = self.0.to_le_bytes();

        let mut end = bytes.len();

        while end > 0 && bytes[end - 1] == 0 {
            end -= 1;
        }

        bytes[..end].into()
    }
}

impl Display for Dune {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let mut n = self.0;
        if n == u128::MAX {
            return write!(f, "BCGDENLQRQWDSLRUGSNLBTMFIJAV");
        }

        n += 1;
        let mut symbol = String::new();
        while n > 0 {
            symbol.push(
                "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
                    .chars()
                    .nth(((n - 1) % 26) as usize)
                    .unwrap(),
            );
            n = (n - 1) / 26;
        }

        for c in symbol.chars().rev() {
            write!(f, "{c}")?;
        }

        Ok(())
    }
}

impl FromStr for Dune {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let mut x = 0u128;
        for (i, c) in s.chars().enumerate() {
            if i > 0 {
                x = x.checked_add(1).ok_or(Error::Range)?;
            }
            x = x.checked_mul(26).ok_or(Error::Range)?;
            match c {
                'A'..='Z' => {
                    x = x.checked_add(c as u128 - 'A' as u128).ok_or(Error::Range)?;
                }
                _ => return Err(Error::Character(c)),
            }
        }
        Ok(Dune(x))
    }
}

#[derive(Debug, PartialEq)]
pub enum Error {
    Character(char),
    Range,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Character(c) => write!(f, "invalid character `{c}`"),
            Self::Range => write!(f, "name out of range"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        fn case(n: u128, s: &str) {
            assert_eq!(Dune(n).to_string(), s);
            assert_eq!(s.parse::<Dune>().unwrap(), Dune(n));
        }

        case(0, "A");
        case(1, "B");
        case(2, "C");
        case(3, "D");
        case(4, "E");
        case(5, "F");
        case(6, "G");
        case(7, "H");
        case(8, "I");
        case(9, "J");
        case(10, "K");
        case(11, "L");
        case(12, "M");
        case(13, "N");
        case(14, "O");
        case(15, "P");
        case(16, "Q");
        case(17, "R");
        case(18, "S");
        case(19, "T");
        case(20, "U");
        case(21, "V");
        case(22, "W");
        case(23, "X");
        case(24, "Y");
        case(25, "Z");
        case(26, "AA");
        case(27, "AB");
        case(51, "AZ");
        case(52, "BA");
        case(u128::MAX - 2, "BCGDENLQRQWDSLRUGSNLBTMFIJAT");
        case(u128::MAX - 1, "BCGDENLQRQWDSLRUGSNLBTMFIJAU");
        case(u128::MAX, "BCGDENLQRQWDSLRUGSNLBTMFIJAV");
    }

    #[test]
    fn from_str_error() {
        assert_eq!(
            "BCGDENLQRQWDSLRUGSNLBTMFIJAW".parse::<Dune>().unwrap_err(),
            Error::Range,
        );
        assert_eq!(
            "BCGDENLQRQWDSLRUGSNLBTMFIJAVX".parse::<Dune>().unwrap_err(),
            Error::Range,
        );
        assert_eq!("x".parse::<Dune>().unwrap_err(), Error::Character('x'));
    }

    #[test]
    #[allow(clippy::identity_op)]
    #[allow(clippy::erasing_op)]
    #[allow(clippy::zero_prefixed_literal)]
    fn mainnet_minimum_at_height() {
        #[track_caller]
        fn case(height: u32, minimum: &str) {
            let minimum = minimum.parse::<Dune>().unwrap();
            assert_eq!(
                Dune::minimum_at_height(Network::Bitcoin, Height(height)),
                minimum,
            );

            let unlock_height = minimum.unlock_height(Network::Bitcoin).unwrap().0;

            assert!(unlock_height <= height);

            if unlock_height == 0 {
                assert!(height < SUBSIDY_HALVING_INTERVAL * 4);
            }
        }

        const START: u32 = SUBSIDY_HALVING_INTERVAL * 4;
        const END: u32 = START + SUBSIDY_HALVING_INTERVAL;

        case(0, "AAAAAAAAAAAAA");
        case(START / 2, "AAAAAAAAAAAAA");
        case(START, "ZZYZXBRKWXVA");
        case(START + 1, "ZZXZUDIVTVQA");
        case(END - 1, "A");
        case(END, "A");
        case(END + 1, "A");
        case(u32::MAX, "A");

        case(START + Dune::UNLOCK_INTERVAL * 00 - 1, "AAAAAAAAAAAAA");
        case(START + Dune::UNLOCK_INTERVAL * 00 + 0, "ZZYZXBRKWXVA");
        case(START + Dune::UNLOCK_INTERVAL * 00 + 1, "ZZXZUDIVTVQA");

        case(START + Dune::UNLOCK_INTERVAL * 01 - 1, "AAAAAAAAAAAA");
        case(START + Dune::UNLOCK_INTERVAL * 01 + 0, "ZZYZXBRKWXV");
        case(START + Dune::UNLOCK_INTERVAL * 01 + 1, "ZZXZUDIVTVQ");

        case(START + Dune::UNLOCK_INTERVAL * 02 - 1, "AAAAAAAAAAA");
        case(START + Dune::UNLOCK_INTERVAL * 02 + 0, "ZZYZXBRKWY");
        case(START + Dune::UNLOCK_INTERVAL * 02 + 1, "ZZXZUDIVTW");

        case(START + Dune::UNLOCK_INTERVAL * 03 - 1, "AAAAAAAAAA");
        case(START + Dune::UNLOCK_INTERVAL * 03 + 0, "ZZYZXBRKX");
        case(START + Dune::UNLOCK_INTERVAL * 03 + 1, "ZZXZUDIVU");

        case(START + Dune::UNLOCK_INTERVAL * 04 - 1, "AAAAAAAAA");
        case(START + Dune::UNLOCK_INTERVAL * 04 + 0, "ZZYZXBRL");
        case(START + Dune::UNLOCK_INTERVAL * 04 + 1, "ZZXZUDIW");

        case(START + Dune::UNLOCK_INTERVAL * 05 - 1, "AAAAAAAA");
        case(START + Dune::UNLOCK_INTERVAL * 05 + 0, "ZZYZXBS");
        case(START + Dune::UNLOCK_INTERVAL * 05 + 1, "ZZXZUDJ");

        case(START + Dune::UNLOCK_INTERVAL * 06 - 1, "AAAAAAA");
        case(START + Dune::UNLOCK_INTERVAL * 06 + 0, "ZZYZXC");
        case(START + Dune::UNLOCK_INTERVAL * 06 + 1, "ZZXZUE");

        case(START + Dune::UNLOCK_INTERVAL * 07 - 1, "AAAAAA");
        case(START + Dune::UNLOCK_INTERVAL * 07 + 0, "ZZYZY");
        case(START + Dune::UNLOCK_INTERVAL * 07 + 1, "ZZXZV");

        case(START + Dune::UNLOCK_INTERVAL * 08 - 1, "AAAAA");
        case(START + Dune::UNLOCK_INTERVAL * 08 + 0, "ZZZA");
        case(START + Dune::UNLOCK_INTERVAL * 08 + 1, "ZZYA");

        case(START + Dune::UNLOCK_INTERVAL * 09 - 1, "AAAA");
        case(START + Dune::UNLOCK_INTERVAL * 09 + 0, "ZZZ");
        case(START + Dune::UNLOCK_INTERVAL * 09 + 1, "ZZY");

        case(START + Dune::UNLOCK_INTERVAL * 10 - 2, "AAC");
        case(START + Dune::UNLOCK_INTERVAL * 10 - 1, "AAA");
        case(START + Dune::UNLOCK_INTERVAL * 10 + 0, "AAA");
        case(START + Dune::UNLOCK_INTERVAL * 10 + 1, "AAA");

        case(
            START + Dune::UNLOCK_INTERVAL * 10 + Dune::UNLOCK_INTERVAL / 2,
            "NA",
        );

        case(START + Dune::UNLOCK_INTERVAL * 11 - 2, "AB");
        case(START + Dune::UNLOCK_INTERVAL * 11 - 1, "AA");
        case(START + Dune::UNLOCK_INTERVAL * 11 + 0, "AA");
        case(START + Dune::UNLOCK_INTERVAL * 11 + 1, "AA");

        case(
            START + Dune::UNLOCK_INTERVAL * 11 + Dune::UNLOCK_INTERVAL / 2,
            "N",
        );

        case(START + Dune::UNLOCK_INTERVAL * 12 - 2, "B");
        case(START + Dune::UNLOCK_INTERVAL * 12 - 1, "A");
        case(START + Dune::UNLOCK_INTERVAL * 12 + 0, "A");
        case(START + Dune::UNLOCK_INTERVAL * 12 + 1, "A");
    }

    #[test]
    fn minimum_at_height() {
        #[track_caller]
        fn case(network: Network, height: u32, minimum: &str) {
            assert_eq!(
                Dune::minimum_at_height(network, Height(height)).to_string(),
                minimum,
            );
        }

        case(Network::Testnet, 0, "AAAAAAAAAAAAA");
        case(
            Network::Testnet,
            SUBSIDY_HALVING_INTERVAL * 12 - 1,
            "AAAAAAAAAAAAA",
        );
        case(
            Network::Testnet,
            SUBSIDY_HALVING_INTERVAL * 12,
            "ZZYZXBRKWXVA",
        );
        case(
            Network::Testnet,
            SUBSIDY_HALVING_INTERVAL * 12 + 1,
            "ZZXZUDIVTVQA",
        );

        case(Network::Signet, 0, "ZZYZXBRKWXVA");
        case(Network::Signet, 1, "ZZXZUDIVTVQA");

        case(Network::Regtest, 0, "ZZYZXBRKWXVA");
        case(Network::Regtest, 1, "ZZXZUDIVTVQA");
    }

    #[test]
    fn serde() {
        let dune = Dune(0);
        let json = "\"A\"";
        assert_eq!(serde_json::to_string(&dune).unwrap(), json);
        assert_eq!(serde_json::from_str::<Dune>(json).unwrap(), dune);
    }

    #[test]
    fn reserved() {
        assert_eq!(
            Dune::RESERVED,
            "AAAAAAAAAAAAAAAAAAAAAAAAAAA".parse::<Dune>().unwrap().0,
        );

        assert_eq!(Dune::reserved(0, 0), Dune(Dune::RESERVED));
        assert_eq!(Dune::reserved(0, 1), Dune(Dune::RESERVED + 1));
        assert_eq!(Dune::reserved(1, 0), Dune(Dune::RESERVED + (1 << 32)));
        assert_eq!(Dune::reserved(1, 1), Dune(Dune::RESERVED + (1 << 32) + 1));
        assert_eq!(
            Dune::reserved(u64::MAX, u32::MAX),
            Dune(Dune::RESERVED + ((u128::from(u64::MAX) << 32) | u128::from(u32::MAX))),
        );
    }

    #[test]
    fn is_reserved() {
        #[track_caller]
        fn case(dune: &str, reserved: bool) {
            let dune = dune.parse::<Dune>().unwrap();
            assert_eq!(dune.is_reserved(), reserved);
            assert_eq!(dune.unlock_height(Network::Bitcoin).is_none(), reserved);
        }

        case("A", false);
        case("ZZZZZZZZZZZZZZZZZZZZZZZZZZ", false);
        case("AAAAAAAAAAAAAAAAAAAAAAAAAAA", true);
        case("AAAAAAAAAAAAAAAAAAAAAAAAAAB", true);
        case("BCGDENLQRQWDSLRUGSNLBTMFIJAV", true);
    }

    #[test]
    fn steps() {
        for i in 0.. {
            match "A".repeat(i + 1).parse::<Dune>() {
                Ok(dune) => assert_eq!(Dune(Dune::STEPS[i]), dune),
                Err(_) => {
                    assert_eq!(Dune::STEPS.len(), i);
                    break;
                }
            }
        }
    }

    #[test]
    fn commitment() {
        #[track_caller]
        fn case(dune: u128, bytes: &[u8]) {
            assert_eq!(Dune(dune).commitment(), bytes);
        }

        case(0, &[]);
        case(1, &[1]);
        case(255, &[255]);
        case(256, &[0, 1]);
        case(65535, &[255, 255]);
        case(65536, &[0, 0, 1]);
        case(u128::MAX, &[255; 16]);
    }

    #[test]
    fn steps_are_sorted_and_unique() {
        let mut steps = Dune::STEPS.to_vec();
        steps.sort();
        assert_eq!(steps, Dune::STEPS);
        steps.dedup();
        assert_eq!(steps, Dune::STEPS);
    }

    #[test]
    fn reserved_dune_unlock_height() {
        assert_eq!(Dune(Dune::RESERVED).unlock_height(Network::Bitcoin), None);
        assert_eq!(
            Dune(Dune::RESERVED + 1).unlock_height(Network::Bitcoin),
            None
        );
        assert_eq!(
            Dune(Dune::RESERVED - 1).unlock_height(Network::Bitcoin),
            Some(Height(0))
        );
    }

    #[test]
    fn unlock_height() {
        #[track_caller]
        fn case(dune: &str, unlock_height: u32) {
            let dune = dune.parse::<Dune>().unwrap();
            assert_eq!(
                dune.unlock_height(Network::Bitcoin),
                Some(Height(unlock_height)),
                "invalid unlock height for dune `{dune}`",
            );

            if unlock_height > 0 {
                assert!(dune >= Dune::minimum_at_height(Network::Bitcoin, Height(unlock_height)));
                assert!(
                    dune < Dune::minimum_at_height(Network::Bitcoin, Height(unlock_height - 1))
                );
            }
        }

        const START: u32 = SUBSIDY_HALVING_INTERVAL * 4;

        case("AAAAAAAAAAAAB", 0);

        case("AAAAAAAAAAAAA", 0);

        case("ZZZZZZZZZZZZ", START);

        case("ZZZZZZZZZZZ", START + Dune::UNLOCK_INTERVAL);

        case("ZZZZZZZZZZ", START + Dune::UNLOCK_INTERVAL * 2);

        case("ZZZZZZZZZ", START + Dune::UNLOCK_INTERVAL * 3);

        case("ZZYZXBRKWXVA", START);

        case("ZZZ", 997_500);

        case("AAA", 1_014_999);

        case("NNNN", 988_400);

        case("Z", 1_033_173);
        case("Y", 1_033_846);
        case("P", 1_039_903);
        case("O", 1_040_576);
        case("N", 1_041_249);
        case("M", 1_041_923);
        case("L", 1_042_596);
        case("K", 1_043_269);
        case("J", 1_043_942);
        case("I", 1_044_615);
        case("H", 1_045_288);
        case("G", 1_045_961);
        case("F", 1_046_634);
        case("E", 1_047_307);
        case("D", 1_047_980);
        case("C", 1_048_653);
        case("B", 1_049_326);
        case("A", 1_049_999);

        for i in 0..4 {
            for n in Dune::STEPS[i]..Dune::STEPS[i + 1] {
                let dune = Dune(n);
                let unlock_height = dune.unlock_height(Network::Bitcoin).unwrap();
                assert!(dune >= Dune::minimum_at_height(Network::Bitcoin, unlock_height));
                assert!(
                    dune < Dune::minimum_at_height(Network::Bitcoin, Height(unlock_height.0 - 1))
                );
            }
        }
    }
}
