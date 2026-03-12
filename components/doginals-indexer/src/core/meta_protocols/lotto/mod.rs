//! DogeLotto meta-protocol parser.
//!
//! Inscriptions with `"p": "DogeLotto"` carry lottery operations.
//! All inscriptions are `text/plain` JSON.
//!
//! Deploys define a template, the draw block, number drums, and payout rules.
//! Tickets only carry a single main-drum seed set and must later be validated
//! against the deployed lottery template.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub const LOTTO_PROTOCOL: &str = "DogeLotto";
pub const DEFAULT_MAIN_PICK: u16 = 69;
pub const GLOBAL_NUMBER_MIN: u16 = 1;
pub const GLOBAL_NUMBER_MAX: u16 = 420;

// closest_fingerprint constants
pub const CLASSIC_PICK: u16 = 6;
pub const CLASSIC_MAX: u16 = 49;
/// Payout tiers in basis points for closest_fingerprint mode:
/// rank 1 = 55%, rank 2 = 20%, rank 3 = 10%, ranks 4-10 pool = 5%.
/// Remaining 10% rolls over (FINGERPRINT_ROLLOVER_BPS).
pub const FINGERPRINT_TIER_BPS: [u32; 4] = [5_500, 2_000, 1_000, 500];
pub const FINGERPRINT_ROLLOVER_BPS: u32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberConfig {
    pub pick: u16,
    pub max: u16,
}

impl NumberConfig {
    pub fn is_disabled(&self) -> bool {
        self.pick == 0 && self.max == 0
    }

    pub fn has_numbers(&self) -> bool {
        self.pick > 0 && self.max > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LottoDraw {
    pub main_numbers: Vec<u16>,
    pub bonus_numbers: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LottoTemplate {
    #[serde(rename = "closest_wins")]
    ClosestWins,
    #[serde(rename = "powerball_dual_drum")]
    PowerballDualDrum,
    #[serde(rename = "6_49_classic")]
    Six49Classic,
    #[serde(rename = "rollover_jackpot")]
    RolloverJackpot,
    #[serde(rename = "always_winner")]
    AlwaysWinner,
    #[serde(rename = "life_annuity")]
    LifeAnnuity,
    #[serde(rename = "custom")]
    Custom,
    /// SHA256-fingerprint distance ranking (DogeLotto variants: doge-69-420, doge-4-20-flash, doge-max).
    #[serde(rename = "closest_fingerprint")]
    ClosestFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionMode {
    AlwaysWinner,
    ClosestWins,
    ExactOnlyWithRollover,
    /// Rank tickets by |SHA256(sorted seed u16 pairs) − block_hash_u256|.
    /// Ties at the same distance split that tier equally.
    /// Secondary display sort: inscription_id lexicographic (lex-smaller first).
    ClosestFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LottoMint {
    pub lotto_id: String,
    pub ticket_id: String,
    pub seed_numbers: Vec<u16>,
    pub luck_marks: Option<Vec<u16>>,
    pub tip_percent: u8,
}

#[derive(Deserialize)]
struct RawDeploy {
    p: Option<String>,
    op: Option<String>,
    lotto_id: Option<String>,
    template: Option<LottoTemplate>,
    draw_block: Option<u64>,
    cutoff_block: Option<u64>,
    ticket_price_koinu: Option<u64>,
    prize_pool_address: Option<String>,
    fee_percent: Option<u8>,
    main_numbers: Option<NumberConfig>,
    bonus_numbers: Option<NumberConfig>,
    resolution_mode: Option<ResolutionMode>,
    rollover_enabled: Option<bool>,
    guaranteed_min_prize_koinu: Option<u64>,
}

#[derive(Deserialize)]
struct RawMint {
    p: Option<String>,
    op: Option<String>,
    lotto_id: Option<String>,
    ticket_id: Option<String>,
    seed_numbers: Option<Vec<u16>>,
    luck_marks: Option<Vec<u16>>,
    tip_percent: Option<u8>,
}

pub fn try_parse_lotto_deploy(body: &[u8]) -> Option<LottoDeploy> {
    let raw: RawDeploy = serde_json::from_slice(trimmed_body(body)?).ok()?;

    if raw.p.as_deref()? != LOTTO_PROTOCOL || raw.op.as_deref()? != "deploy" {
        return None;
    }

    let lotto_id = normalize_lotto_id(raw.lotto_id?)?;
    let template = raw.template?;
    let draw_block = raw.draw_block?;
    let cutoff_block = raw
        .cutoff_block
        .unwrap_or_else(|| draw_block.saturating_sub(10));
    let ticket_price_koinu = raw.ticket_price_koinu?;
    let prize_pool_address = normalize_prize_pool_address(raw.prize_pool_address?)?;
    let fee_percent = raw.fee_percent?;
    let main_numbers = normalize_number_config(raw.main_numbers?, false)?;
    let bonus_numbers = normalize_number_config(raw.bonus_numbers?, true)?;
    let resolution_mode = raw.resolution_mode?;
    let rollover_enabled = raw.rollover_enabled?;
    let guaranteed_min_prize_koinu = raw.guaranteed_min_prize_koinu;

    if draw_block == 0
        || cutoff_block == 0
        || cutoff_block >= draw_block
        || ticket_price_koinu == 0
        || fee_percent > 10
    {
        return None;
    }

    if !template_matches_config(
        &template,
        &main_numbers,
        &bonus_numbers,
        &resolution_mode,
        rollover_enabled,
        guaranteed_min_prize_koinu,
    ) {
        return None;
    }

    Some(LottoDeploy {
        lotto_id,
        template,
        draw_block,
        cutoff_block,
        ticket_price_koinu,
        prize_pool_address,
        fee_percent,
        main_numbers,
        bonus_numbers,
        resolution_mode,
        rollover_enabled,
        guaranteed_min_prize_koinu,
    })
}

pub fn try_parse_lotto_mint(body: &[u8]) -> Option<LottoMint> {
    let raw: RawMint = serde_json::from_slice(trimmed_body(body)?).ok()?;

    if raw.p.as_deref()? != LOTTO_PROTOCOL || raw.op.as_deref()? != "mint" {
        return None;
    }

    let lotto_id = normalize_lotto_id(raw.lotto_id?)?;
    let ticket_id = normalize_ticket_id(raw.ticket_id?)?;
    let luck_marks = raw
        .luck_marks
        .map(|numbers| normalize_seed_numbers(numbers, GLOBAL_NUMBER_MAX))
        .transpose()?;
    let seed_numbers = if let Some(seed_numbers) = raw.seed_numbers {
        normalize_seed_numbers(seed_numbers, GLOBAL_NUMBER_MAX)?
    } else if let Some(luck_marks) = &luck_marks {
        luck_marks.clone()
    } else {
        return None;
    };
    let tip_percent = raw.tip_percent.unwrap_or(0);
    if tip_percent > 10 {
        return None;
    }

    Some(LottoMint {
        lotto_id,
        ticket_id,
        seed_numbers,
        luck_marks,
        tip_percent,
    })
}

pub fn try_parse_lotto_mint_for_deploy(body: &[u8], deploy: &LottoDeploy) -> Option<LottoMint> {
    let mint = try_parse_lotto_mint(body)?;
    if validate_mint_against_deploy(&mint, deploy) {
        Some(mint)
    } else {
        None
    }
}

pub fn validate_mint_against_deploy(mint: &LottoMint, deploy: &LottoDeploy) -> bool {
    mint.lotto_id == deploy.lotto_id
        && validate_seed_numbers_for_config(&mint.seed_numbers, &deploy.main_numbers)
}

pub fn quickpick() -> Vec<u16> {
    quickpick_for_config(&NumberConfig {
        pick: DEFAULT_MAIN_PICK,
        max: GLOBAL_NUMBER_MAX,
    })
}

pub fn quickpick_for_config(config: &NumberConfig) -> Vec<u16> {
    use rand::Rng;

    if !config.has_numbers() {
        return Vec::new();
    }

    let mut pool: Vec<u16> = (GLOBAL_NUMBER_MIN..=config.max).collect();
    let mut rng = rand::rng();
    for i in 0..config.pick as usize {
        let j = rng.random_range(i..pool.len());
        pool.swap(i, j);
    }

    let mut pick = pool[..config.pick as usize].to_vec();
    pick.sort_unstable();
    pick
}

pub fn derive_drawn_numbers(block_hash: &str) -> Vec<u16> {
    derive_numbers_for_config(
        block_hash,
        &NumberConfig {
            pick: DEFAULT_MAIN_PICK,
            max: GLOBAL_NUMBER_MAX,
        },
        "main",
    )
}

pub fn derive_draw_for_deploy(block_hash: &str, deploy: &LottoDeploy) -> LottoDraw {
    LottoDraw {
        main_numbers: derive_numbers_for_config(block_hash, &deploy.main_numbers, "main"),
        bonus_numbers: derive_numbers_for_config(block_hash, &deploy.bonus_numbers, "bonus"),
    }
}

pub fn derive_numbers_for_config(
    block_hash: &str,
    config: &NumberConfig,
    domain_separator: &str,
) -> Vec<u16> {
    if !config.has_numbers() {
        return Vec::new();
    }

    let mut raw = hex::decode(block_hash.trim_start_matches("0x"))
        .unwrap_or_else(|_| block_hash.as_bytes().to_vec());
    raw.extend_from_slice(domain_separator.as_bytes());

    while raw.len() < config.pick as usize * 4 {
        let next = pseudo_hash(&raw);
        raw.extend_from_slice(&next);
    }

    let mut seen: HashSet<u16> = HashSet::with_capacity(config.pick as usize);
    let mut result = Vec::with_capacity(config.pick as usize);
    let mut i = 0;
    while result.len() < config.pick as usize && i + 1 < raw.len() {
        let number = (u16::from_be_bytes([raw[i], raw[i + 1]]) % config.max) + GLOBAL_NUMBER_MIN;
        if seen.insert(number) {
            result.push(number);
        }
        i += 1;
    }
    result.sort_unstable();
    result
}

/// SHA256 of seed_numbers sorted as big-endian u16 pairs → 32-byte fingerprint (u256).
pub fn compute_ticket_fingerprint(seed_numbers: &[u16]) -> [u8; 32] {
    use bitcoin::hashes::{sha256, Hash, HashEngine};

    let mut sorted = seed_numbers.to_vec();
    sorted.sort_unstable();

    let mut engine = sha256::HashEngine::default();
    for n in &sorted {
        engine.input(&n.to_be_bytes());
    }
    *sha256::Hash::from_engine(engine).as_byte_array()
}

/// Unsigned 256-bit absolute difference: |a − b| with big-endian [u8; 32].
pub fn u256_abs_diff(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    // Determine which is larger
    if a >= b {
        u256_sub(a, b)
    } else {
        u256_sub(b, a)
    }
}

fn u256_sub(larger: &[u8; 32], smaller: &[u8; 32]) -> [u8; 32] {
    let mut result = [0u8; 32];
    let mut borrow: u16 = 0;
    for i in (0..32).rev() {
        let diff = larger[i] as i16 - smaller[i] as i16 - borrow as i16;
        if diff < 0 {
            result[i] = (diff + 256) as u8;
            borrow = 1;
        } else {
            result[i] = diff as u8;
            borrow = 0;
        }
    }
    result
}

/// Derive 6 classic numbers (1-49) deterministically from a ticket fingerprint.
pub fn derive_classic_numbers(fingerprint: &[u8; 32]) -> Vec<u16> {
    let hex = hex::encode(fingerprint);
    derive_numbers_for_config(
        &hex,
        &NumberConfig {
            pick: CLASSIC_PICK,
            max: CLASSIC_MAX,
        },
        "classic",
    )
}

/// Derive 6 classic numbers drawn at resolution from the draw block hash.
pub fn derive_classic_drawn_numbers(block_hash: &str) -> Vec<u16> {
    derive_numbers_for_config(
        block_hash,
        &NumberConfig {
            pick: CLASSIC_PICK,
            max: CLASSIC_MAX,
        },
        "classic",
    )
}

/// Count how many ticket classic numbers appear in the drawn classic numbers.
pub fn count_classic_matches(ticket_classic: &[u16], drawn_classic: &[u16]) -> usize {
    let drawn_set: HashSet<u16> = drawn_classic.iter().copied().collect();
    ticket_classic
        .iter()
        .filter(|n| drawn_set.contains(n))
        .count()
}

/// Fixed classic-tier prize multiplier in koinu per match count (0 if below threshold).
/// Returns a basis-points fraction of the dedicated classic pool (handled by caller).
/// 3 matches → 1_000 bps, 4 → 2_000 bps, 5 → 4_000 bps, 6 → 10_000 bps.
pub fn classic_prize_bps(matches: usize) -> u32 {
    match matches {
        3 => 1_000,
        4 => 2_000,
        5 => 4_000,
        6 => 10_000,
        _ => 0,
    }
}

pub fn score_ticket(seed_numbers: &[u16], drawn: &[u16]) -> u64 {
    drawn
        .iter()
        .map(|&drawn_number| {
            seed_numbers
                .iter()
                .map(|&seed| (drawn_number as i32 - seed as i32).unsigned_abs() as u64)
                .min()
                .unwrap_or(u64::MAX / 2)
        })
        .sum()
}

fn trimmed_body(body: &[u8]) -> Option<&[u8]> {
    let text = std::str::from_utf8(body).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.as_bytes())
}

fn normalize_lotto_id(lotto_id: String) -> Option<String> {
    let lotto_id = lotto_id.trim();
    if lotto_id.is_empty() || lotto_id.len() > 64 {
        return None;
    }
    if !lotto_id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return None;
    }
    Some(lotto_id.to_string())
}

fn normalize_prize_pool_address(address: String) -> Option<String> {
    let address = address.trim();
    if address.len() < 24 || address.len() > 128 {
        return None;
    }
    if address
        .chars()
        .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
    {
        return None;
    }
    Some(address.to_string())
}

fn normalize_number_config(config: NumberConfig, allow_disabled: bool) -> Option<NumberConfig> {
    if config.is_disabled() {
        return allow_disabled.then_some(config);
    }

    if config.pick == 0 || config.max == 0 {
        return None;
    }
    if config.max > GLOBAL_NUMBER_MAX || config.pick > config.max {
        return None;
    }

    Some(config)
}

fn normalize_ticket_id(ticket_id: String) -> Option<String> {
    let ticket_id = ticket_id.trim();
    if ticket_id.is_empty() || ticket_id.len() > 128 {
        return None;
    }
    if ticket_id.chars().any(|c| c.is_ascii_control()) {
        return None;
    }
    Some(ticket_id.to_string())
}

fn normalize_seed_numbers(mut seed_numbers: Vec<u16>, max_number: u16) -> Option<Vec<u16>> {
    if seed_numbers.is_empty() || seed_numbers.len() > max_number as usize {
        return None;
    }
    if seed_numbers
        .iter()
        .any(|&number| !(GLOBAL_NUMBER_MIN..=max_number).contains(&number))
    {
        return None;
    }

    let original_len = seed_numbers.len();
    seed_numbers.sort_unstable();
    seed_numbers.dedup();
    if seed_numbers.len() != original_len {
        return None;
    }

    Some(seed_numbers)
}

fn validate_seed_numbers_for_config(seed_numbers: &[u16], config: &NumberConfig) -> bool {
    if !config.has_numbers() || seed_numbers.len() != config.pick as usize {
        return false;
    }

    let mut seen = HashSet::with_capacity(seed_numbers.len());
    seed_numbers
        .iter()
        .all(|number| (GLOBAL_NUMBER_MIN..=config.max).contains(number) && seen.insert(*number))
}

fn template_matches_config(
    template: &LottoTemplate,
    main_numbers: &NumberConfig,
    bonus_numbers: &NumberConfig,
    resolution_mode: &ResolutionMode,
    rollover_enabled: bool,
    guaranteed_min_prize_koinu: Option<u64>,
) -> bool {
    if !main_numbers.has_numbers() {
        return false;
    }

    match template {
        LottoTemplate::ClosestWins => {
            bonus_numbers.is_disabled() && matches!(resolution_mode, ResolutionMode::ClosestWins)
        }
        LottoTemplate::AlwaysWinner => {
            bonus_numbers.is_disabled() && matches!(resolution_mode, ResolutionMode::AlwaysWinner)
        }
        LottoTemplate::RolloverJackpot => {
            matches!(resolution_mode, ResolutionMode::ExactOnlyWithRollover) && rollover_enabled
        }
        LottoTemplate::PowerballDualDrum => {
            bonus_numbers.has_numbers()
                && matches!(resolution_mode, ResolutionMode::ExactOnlyWithRollover)
        }
        LottoTemplate::Six49Classic => {
            bonus_numbers.is_disabled()
                && matches!(resolution_mode, ResolutionMode::ExactOnlyWithRollover)
        }
        LottoTemplate::LifeAnnuity => guaranteed_min_prize_koinu.is_some(),
        LottoTemplate::Custom => true,
        LottoTemplate::ClosestFingerprint => {
            bonus_numbers.is_disabled()
                && matches!(resolution_mode, ResolutionMode::ClosestFingerprint)
        }
    }
}

fn pseudo_hash(input: &[u8]) -> Vec<u8> {
    use std::num::Wrapping;

    let mut h: [Wrapping<u32>; 8] = [
        Wrapping(0x6a09e667u32),
        Wrapping(0xbb67ae85),
        Wrapping(0x3c6ef372),
        Wrapping(0xa54ff53a),
        Wrapping(0x510e527f),
        Wrapping(0x9b05688c),
        Wrapping(0x1f83d9ab),
        Wrapping(0x5be0cd19),
    ];

    for (i, &byte) in input.iter().enumerate() {
        let word = Wrapping(byte as u32);
        h[i % 8] = h[i % 8] ^ (word << (i % 31)) ^ (h[(i + 1) % 8] >> 17);
    }

    let mut out = Vec::with_capacity(32);
    for word in &h {
        out.extend_from_slice(&word.0.to_be_bytes());
    }
    out
}
