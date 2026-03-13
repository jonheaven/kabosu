//! Dogetag on-chain graffiti protocol.
//!
//! A Dogetag is any transaction output that carries an OP_RETURN (`0x6a`) with
//! valid UTF-8 text.  It is NOT an inscription — no backward traversal, no
//! wallet ownership, just a permanent mark in the chain's transaction history.
//!
//! Detection rules:
//! - Output scriptPubKey must start with `6a` (OP_RETURN opcode, hex).
//! - The bytes following the push-data opcode must be valid UTF-8.
//! - Message must be non-empty, free of null bytes, and ≤ 80 bytes (standard
//!   OP_RETURN limit that Dogecoin nodes relay by default).

/// Maximum Dogetag message length in bytes.
pub const MAX_DOGETAG_BYTES: usize = 80;

/// Attempt to extract a Dogetag message from a `scriptPubKey` hex string.
///
/// `script_hex` is the raw hex of the output script (without a `0x` prefix).
/// Returns `Some(message)` for valid dogetags, `None` otherwise.
pub fn try_parse_dogetag(script_hex: &str) -> Option<String> {
    // Strip optional "0x" prefix that kabosu uses for script fields.
    let hex = script_hex.strip_prefix("0x").unwrap_or(script_hex);

    // Must start with OP_RETURN (6a).
    if !hex.starts_with("6a") {
        return None;
    }

    // Decode the full script.
    let script_bytes = hex::decode(hex).ok()?;
    if script_bytes.len() < 2 {
        return None;
    }

    // script_bytes[0] == 0x6a (OP_RETURN)
    // script_bytes[1] is the push-data opcode / length byte.
    // We support direct push (1–75 bytes) and OP_PUSHDATA1 (0x4c, 1 byte length follows).
    let data: &[u8] = match script_bytes[1] {
        0x00 => return None, // empty push
        n if n <= 0x4b => {
            // Direct push: the byte itself is the length.
            let len = n as usize;
            if 2 + len > script_bytes.len() {
                return None;
            }
            &script_bytes[2..2 + len]
        }
        0x4c => {
            // OP_PUSHDATA1: next byte is the length.
            if script_bytes.len() < 3 {
                return None;
            }
            let len = script_bytes[2] as usize;
            if 3 + len > script_bytes.len() {
                return None;
            }
            &script_bytes[3..3 + len]
        }
        _ => return None, // OP_PUSHDATA2/4 exceed the 80-byte relay limit anyway
    };

    if data.is_empty() || data.len() > MAX_DOGETAG_BYTES {
        return None;
    }

    // Must be valid UTF-8, no null bytes.
    let message = std::str::from_utf8(data).ok()?;
    let message = message.trim();
    if message.is_empty() || message.contains('\u{0000}') {
        return None;
    }

    Some(message.to_string())
}
