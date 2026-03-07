/// Dogemap (Dogecoin Bitmap) detection.
///
/// Detection rules (mirrors dog's `index_dogemap_transaction`):
///   - Inscription body must be valid UTF-8 text (after trim).
///   - Must end with `.dogemap`.
///   - The prefix must be a non-empty string of ASCII digits.
///   - Parse prefix as u32 — the target block number.
///   - Target block must be ≤ the current block height (can't claim the future).
///   - First inscription wins (across blocks via SQL `ON CONFLICT DO NOTHING`;
///     within a block via `HashMap::entry().or_insert()`).

/// Attempt to parse `body` as a Dogemap claim.
///
/// `current_block_height` — the height of the block being indexed.
///
/// Returns `Some(block_number)` on a valid candidate, `None` otherwise.
pub fn try_parse_dogemap_claim(body: &[u8], current_block_height: u64) -> Option<u32> {
    let text = std::str::from_utf8(body).ok()?.trim();
    let prefix = text.strip_suffix(".dogemap")?;
    if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let block_number: u32 = prefix.parse().ok()?;
    // Bitmap spec: can only claim a block that already exists
    if block_number as u64 > current_block_height {
        return None;
    }
    Some(block_number)
}
