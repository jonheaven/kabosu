use super::*;

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Ord,
    PartialOrd,
    Eq,
    Default,
    DeserializeFromStr,
    SerializeDisplay,
)]
pub struct SpacedDune {
    pub dune: Dune,
    pub spacers: u32,
}

impl SpacedDune {
    pub fn new(dune: Dune, spacers: u32) -> Self {
        Self { dune, spacers }
    }
}

impl FromStr for SpacedDune {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut dune = String::new();
        let mut spacers = 0u32;

        for c in s.chars() {
            match c {
                'A'..='Z' => dune.push(c),
                '.' | '•' => {
                    let flag = 1 << dune.len().checked_sub(1).ok_or(Error::LeadingSpacer)?;
                    if spacers & flag != 0 {
                        return Err(Error::DoubleSpacer);
                    }
                    spacers |= flag;
                }
                _ => return Err(Error::Character(c)),
            }
        }

        if 32 - spacers.leading_zeros() >= dune.len().try_into().unwrap() {
            return Err(Error::TrailingSpacer);
        }

        Ok(SpacedDune {
            dune: dune.parse().map_err(Error::Dune)?,
            spacers,
        })
    }
}

impl Display for SpacedDune {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let dune = self.dune.to_string();

        for (i, c) in dune.chars().enumerate() {
            write!(f, "{c}")?;

            if i < dune.len() - 1 && self.spacers & (1 << i) != 0 {
                write!(f, "•")?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub enum Error {
    LeadingSpacer,
    TrailingSpacer,
    DoubleSpacer,
    Character(char),
    Dune(dune::Error),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Character(c) => write!(f, "invalid character `{c}`"),
            Self::DoubleSpacer => write!(f, "double spacer"),
            Self::LeadingSpacer => write!(f, "leading spacer"),
            Self::TrailingSpacer => write!(f, "trailing spacer"),
            Self::Dune(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!("A.B".parse::<SpacedDune>().unwrap().to_string(), "A•B");
        assert_eq!("A.B.C".parse::<SpacedDune>().unwrap().to_string(), "A•B•C");
        assert_eq!(
            SpacedDune {
                dune: Dune(0),
                spacers: 1
            }
            .to_string(),
            "A"
        );
    }

    #[test]
    fn from_str() {
        #[track_caller]
        fn case(s: &str, dune: &str, spacers: u32) {
            assert_eq!(
                s.parse::<SpacedDune>().unwrap(),
                SpacedDune {
                    dune: dune.parse().unwrap(),
                    spacers
                },
            );
        }

        assert_eq!(
            ".A".parse::<SpacedDune>().unwrap_err(),
            Error::LeadingSpacer,
        );

        assert_eq!(
            "A..B".parse::<SpacedDune>().unwrap_err(),
            Error::DoubleSpacer,
        );

        assert_eq!(
            "A.".parse::<SpacedDune>().unwrap_err(),
            Error::TrailingSpacer,
        );

        assert_eq!(
            "Ax".parse::<SpacedDune>().unwrap_err(),
            Error::Character('x')
        );

        case("A.B", "AB", 0b1);
        case("A.B.C", "ABC", 0b11);
        case("A•B", "AB", 0b1);
        case("A•B•C", "ABC", 0b11);
        case("A•BC", "ABC", 0b1);
    }

    #[test]
    fn serde() {
        let spaced_dune = SpacedDune {
            dune: Dune(26),
            spacers: 1,
        };
        let json = "\"A•A\"";
        assert_eq!(serde_json::to_string(&spaced_dune).unwrap(), json);
        assert_eq!(
            serde_json::from_str::<SpacedDune>(json).unwrap(),
            spaced_dune
        );
    }
}
