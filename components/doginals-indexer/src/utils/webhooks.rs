use dogecoin::{try_warn, utils::Context};
use reqwest::Client;
use serde_json::Value;

/// Fire-and-forget: POST `payload` to every URL in `urls`.
/// Errors are logged as warnings — a failed delivery never blocks indexing.
pub async fn fire_webhooks(urls: &[String], payload: Value, ctx: &Context) {
    if urls.is_empty() {
        return;
    }
    let client = Client::new();
    for url in urls {
        match client.post(url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                try_warn!(ctx, "Webhook POST to {url} returned status {}", resp.status());
            }
            Err(e) => {
                try_warn!(ctx, "Webhook POST to {url} failed: {e}");
            }
        }
    }
}

/// Build a DNS registration event payload.
pub fn dns_event(
    name: &str,
    inscription_id: &str,
    block_height: u64,
    block_timestamp: u32,
) -> Value {
    serde_json::json!({
        "event": "dns.registered",
        "name": name,
        "inscription_id": inscription_id,
        "block_height": block_height,
        "block_timestamp": block_timestamp,
    })
}

/// Build a Dogemap claim event payload.
pub fn dogemap_event(
    block_number: u32,
    inscription_id: &str,
    claim_height: u64,
    claim_timestamp: u32,
) -> Value {
    serde_json::json!({
        "event": "dogemap.claimed",
        "block_number": block_number,
        "inscription_id": inscription_id,
        "claim_height": claim_height,
        "claim_timestamp": claim_timestamp,
    })
}

/// Build a doge-lotto ticket event payload.
pub fn lotto_ticket_event(
    lotto_id: &str,
    ticket_id: &str,
    inscription_id: &str,
    tx_id: &str,
    minted_height: u64,
    minted_timestamp: u64,
    seed_numbers: &[u16],
    tip_percent: u8,
) -> Value {
    serde_json::json!({
        "event": "lotto.ticket_minted",
        "lotto_id": lotto_id,
        "ticket_id": ticket_id,
        "inscription_id": inscription_id,
        "tx_id": tx_id,
        "minted_height": minted_height,
        "minted_timestamp": minted_timestamp,
        "seed_numbers": seed_numbers,
        "tip_percent": tip_percent,
    })
}

/// Build a doge-lotto winner resolution event payload.
pub fn lotto_winner_event(
    lotto_id: &str,
    ticket_id: &str,
    inscription_id: &str,
    resolved_height: u64,
    rank: u32,
    score: u64,
    payout_bps: u32,
    gross_payout_koinu: u64,
    tip_percent: u8,
    tip_deduction_koinu: u64,
    payout_koinu: u64,
    seed_numbers: &[u16],
    drawn_numbers: &[u16],
) -> Value {
    serde_json::json!({
        "event": "lotto.winner_resolved",
        "lotto_id": lotto_id,
        "ticket_id": ticket_id,
        "inscription_id": inscription_id,
        "resolved_height": resolved_height,
        "rank": rank,
        "score": score,
        "payout_bps": payout_bps,
        "gross_payout_koinu": gross_payout_koinu,
        "tip_percent": tip_percent,
        "tip_deduction_koinu": tip_deduction_koinu,
        "payout_koinu": payout_koinu,
        "seed_numbers": seed_numbers,
        "drawn_numbers": drawn_numbers,
    })
}
