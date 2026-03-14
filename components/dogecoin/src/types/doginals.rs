use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum DoginalOperation {
    InscriptionRevealed(DoginalInscriptionRevealData),
    InscriptionTransferred(DoginalInscriptionTransferData),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct DoginalInscriptionTransferData {
    pub doginal_number: u64,
    pub destination: DoginalInscriptionTransferDestination,
    pub koinupoint_pre_transfer: String,
    pub koinupoint_post_transfer: String,
    pub post_transfer_output_value: Option<u64>,
    pub tx_index: usize,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum DoginalInscriptionTransferDestination {
    Transferred(String),
    SpentInFees,
    Burnt(String),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub enum DoginalInscriptionCurseType {
    DuplicateField,
    IncompleteField,
    NotAtOffsetZero,
    NotInFirstInput,
    Pointer,
    Pushnum,
    Reinscription,
    Stutter,
    UnrecognizedEvenField,
    Generic,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct DoginalInscriptionRevealData {
    pub content_bytes: String,
    pub content_type: String,
    pub content_length: usize,
    pub inscription_number: DoginalInscriptionNumber,
    pub inscription_fee: u64,
    pub inscription_output_value: u64,
    pub inscription_id: String,
    pub inscription_input_index: usize,
    pub inscription_pointer: Option<u64>,
    pub inscriber_address: Option<String>,
    pub delegate: Option<String>,
    pub metaprotocol: Option<String>,
    pub metadata: Option<Value>,
    pub parents: Vec<String>,
    pub doginal_number: u64,
    pub doginal_block_height: u64,
    pub doginal_offset: u64,
    pub tx_index: usize,
    pub transfers_pre_inscription: u32,
    pub koinupoint_post_inscription: String,
    pub curse_type: Option<DoginalInscriptionCurseType>,
    pub dogespells: u16,
    pub unbound_sequence: Option<i64>,
}

impl DoginalInscriptionNumber {
    pub fn zero() -> Self {
        DoginalInscriptionNumber {
            jubilee: 0,
            classic: 0,
        }
    }
}

impl DoginalInscriptionRevealData {
    pub fn get_inscription_number(&self) -> i64 {
        self.inscription_number.jubilee
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct DoginalInscriptionNumber {
    pub classic: i64,
    pub jubilee: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Drc20TokenDeployData {
    pub tick: String,
    pub max: String,
    pub lim: String,
    pub dec: String,
    pub address: String,
    pub inscription_id: String,
    pub self_mint: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Drc20BalanceData {
    pub tick: String,
    pub amt: String,
    pub address: String,
    pub inscription_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Drc20TransferData {
    pub tick: String,
    pub amt: String,
    pub sender_address: String,
    pub receiver_address: String,
    pub inscription_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Drc20Operation {
    Deploy(Drc20TokenDeployData),
    Mint(Drc20BalanceData),
    Transfer(Drc20BalanceData),
    TransferSend(Drc20TransferData),
}
