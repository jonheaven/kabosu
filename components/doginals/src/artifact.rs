use super::*;

#[derive(Serialize, Eq, PartialEq, Deserialize, Debug)]
pub enum Artifact {
    Cenotaph(Cenotaph),
    Dunestone(Dunestone),
}

impl Artifact {
    pub fn mint(&self) -> Option<DuneId> {
        match self {
            Self::Cenotaph(cenotaph) => cenotaph.mint,
            Self::Dunestone(dunestone) => dunestone.mint,
        }
    }
}
