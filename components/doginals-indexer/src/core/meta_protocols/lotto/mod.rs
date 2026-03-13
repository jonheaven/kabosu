

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
    #[serde(alias = "gm")]
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
