use super::*;

#[derive(PartialEq, Debug)]
pub struct DecimalKoinu {
    pub height: Height,
    pub offset: u64,
}

impl From<Koinu> for DecimalKoinu {
    fn from(sat: Koinu) -> Self {
        Self {
            height: sat.height(),
            offset: sat.third(),
        }
    }
}

impl Display for DecimalKoinu {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}.{}", self.height, self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal() {
        assert_eq!(
            Koinu(0).decimal(),
            DecimalKoinu {
                height: Height(0),
                offset: 0
            }
        );
        assert_eq!(
            Koinu(1).decimal(),
            DecimalKoinu {
                height: Height(0),
                offset: 1
            }
        );
        assert_eq!(
            Koinu(2099999997689999).decimal(),
            DecimalKoinu {
                height: Height(6929999),
                offset: 0
            }
        );
    }
}
