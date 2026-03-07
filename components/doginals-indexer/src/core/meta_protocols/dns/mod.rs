/// Dogecoin Name System (DNS) detection.
///
/// Detection rules (mirrors dog's `index_dns_transaction`):
///   - Inscription body must be valid UTF-8 text (after trim).
///   - Exactly one dot: `<label>.<namespace>` where both parts are non-empty.
///   - The namespace must be in the canonical Dogecoin namespace list.
///   - First inscription wins (across blocks via SQL `ON CONFLICT DO NOTHING`;
///     within a block via `HashMap::entry().or_insert()`).

/// Returns `true` if `namespace` is a recognised Dogecoin DNS namespace.
/// List kept in sync with dog's `is_valid_dns_namespace`.
pub fn is_valid_dns_namespace(namespace: &str) -> bool {
    matches!(
        namespace,
        "doge"
            | "dogecoin"
            | "shibe"
            | "shib"
            | "wow"
            | "very"
            | "such"
            | "much"
            | "excite"
            | "woof"
            | "bark"
            | "tail"
            | "paws"
            | "paw"
            | "moon"
            | "kabosu"
            | "cheems"
            | "inu"
            | "cook"
            | "doggo"
            | "boop"
            | "zoomies"
            | "smol"
            | "snoot"
            | "pupper"
            | "official"
    )
}

/// Attempt to parse `body` as a DNS name registration.
/// Returns `Some(name)` (e.g. `"satoshi.doge"`) on a valid candidate,
/// `None` otherwise.
pub fn try_parse_dns_name(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?.trim();
    let parts: Vec<&str> = text.splitn(3, '.').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return None;
    }
    let namespace = parts[1];
    if !is_valid_dns_namespace(namespace) {
        return None;
    }
    Some(text.to_string())
}
