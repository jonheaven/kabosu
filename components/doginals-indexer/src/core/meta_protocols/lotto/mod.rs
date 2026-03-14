use rand::rng;
/// DogeLotto meta-protocol structs and fingerprint logic

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LottoDeploy {
    pub lotto_id: String,
    pub template: LottoTemplate,
    pub draw_block: u64,
    pub cutoff_block: u64,
    pub ticket_price_koinu: u64,
    pub prize_pool_address: String,
    pub fee_percent: u8,
    pub main_numbers: NumberConfig,
    pub bonus_numbers: NumberConfig,
    pub resolution_mode: ResolutionMode,
    pub rollover_enabled: bool,
    pub guaranteed_min_prize_koinu: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumberConfig {
    pub pick: u16,
    pub max: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LottoTemplate {
    ClosestWins,
    Six49Classic,
    LifeAnnuity,
    PowerballDualDrum,
    RolloverJackpot,
    AlwaysWinner,
    Custom,
    ClosestFingerprint,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ResolutionMode {
    AlwaysWinner,
    ExactOnlyWithRollover,
    ClosestWins,
    ClosestFingerprint,
}

#[derive(Debug, Clone)]
pub struct LottoDraw {
    pub main_numbers: Vec<u16>,
    pub bonus_numbers: Vec<u16>,
}

pub const GLOBAL_NUMBER_MIN: u16 = 1;
pub const CLASSIC_MAX: u16 = 49;

pub const FINGERPRINT_TIER_BPS: [u32; 4] = [5500, 2000, 1000, 500];

pub fn compute_ticket_fingerprint(seed_numbers: &[u16]) -> [u8; 32] {
    let mut sorted = seed_numbers.to_vec();
    sorted.sort_unstable();
    let input = sorted.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(",");
    let digest = Sha256::digest(input.as_bytes());
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&digest);
    fp
}

pub fn u256_abs_diff(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    if a >= b {
        sub_be_32(a, b)
    } else {
        sub_be_32(b, a)
    }
}

fn sub_be_32(large: &[u8; 32], small: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut value = large[i] as i16 - small[i] as i16 - borrow;
        if value < 0 {
            value += 256;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out[i] = value as u8;
    }
    out
}

pub fn score_ticket(ticket: &[u16], drawn: &[u16]) -> u64 {
    let matches = ticket.iter().filter(|n| drawn.contains(n)).count() as u64;
    matches * matches
}

pub fn count_classic_matches(ticket: &[u16], drawn: &[u16]) -> usize {
    ticket.iter().filter(|n| drawn.contains(n)).count()
}

pub fn derive_classic_numbers(fp_bytes: &[u8]) -> Vec<u16> {
    let mut numbers = Vec::with_capacity(6);
    for i in (0..fp_bytes.len()).step_by(2) {
        if numbers.len() >= 6 { break; }
        let raw = u16::from_be_bytes([fp_bytes[i], fp_bytes[i + 1]]);
        let number = (raw % CLASSIC_MAX) + GLOBAL_NUMBER_MIN;
        if !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    numbers
}

pub fn derive_classic_drawn_numbers(block_hash: &str) -> Vec<u16> {
    let hash_hex = block_hash.trim_start_matches("0x");
    let bytes = hex::decode(hash_hex).unwrap_or_default();
    derive_classic_numbers(&bytes)
}

pub fn derive_draw_for_deploy(block_hash: &str, _deploy: &LottoDeploy) -> LottoDraw {
    let main = derive_classic_drawn_numbers(block_hash);
    LottoDraw {
        main_numbers: main,
        bonus_numbers: vec![],
    }
}

pub fn classic_prize_bps(matches: usize) -> u32 {
    match matches {
        6 => 10000,
        5 => 5000,
        4 => 1000,
        _ => 0,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LottoMint {
    pub lotto_id: String,
    pub ticket_id: String,
    pub seed_numbers: Vec<u16>,
    pub luck_marks: Option<Vec<u16>>,
    pub tip_percent: u8,
}

pub fn try_parse_lotto_deploy(body: &[u8]) -> Option<LottoDeploy> {
    serde_json::from_slice(body).ok()
}

pub fn try_parse_lotto_mint(body: &[u8]) -> Option<LottoMint> {
    serde_json::from_slice(body).ok()
}

pub fn validate_mint_against_deploy(_mint: &LottoMint, _deploy: &LottoDeploy) -> bool {
    true // TODO: real validation later
}

impl NumberConfig {
    pub fn has_numbers(&self) -> bool { self.pick > 0 }
    pub fn is_disabled(&self) -> bool { self.pick == 0 }
}

use rand::prelude::*;

pub fn quickpick_for_config(config: &NumberConfig) -> Vec<u16> {
    if config.pick == 0 {
        return vec![];
    }
    let mut rng = rng();
    let mut numbers = Vec::new();
    while numbers.len() < config.pick as usize {
        let num = rng.random_range(GLOBAL_NUMBER_MIN..=config.max);
        if !numbers.contains(&num) {
            numbers.push(num);
        }
    }
    numbers.sort_unstable();
    numbers
}
