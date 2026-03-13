use super::*;

#[derive(Serialize, Eq, PartialEq, Deserialize, Debug, Default)]
pub struct Cenotaph {
    pub etching: Option<Dune>,
    pub flaw: Option<Flaw>,
    pub mint: Option<DuneId>,
}
