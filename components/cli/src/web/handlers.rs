use std::{
    collections::BTreeMap,
    convert::Infallible,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Redirect, Response,
    },
    Json,
};
use deadpool_postgres::tokio_postgres::Row;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

use bitcoin::{
    base58,
    consensus::encode::deserialize,
    hashes::{sha256d, Hash},
    PubkeyHash, ScriptBuf, ScriptHash, Transaction,
};
use dogecoin::bitcoincore_rpc::RpcApi;
use doginals::envelope::ParsedEnvelope;

use super::{AppState, MonitorBlockSample, MonitorEvent};

const KOINU_RELIC_TEMPLATE: &str = include_str!("../../../../../koinu-relic/src/inscription.html");

#[derive(Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct InscriptionRow {
    pub inscription_id: String,
    pub inscription_number: i64,
    pub block_height: i64,
    pub block_timestamp: i64,
    pub content_type: Option<String>,
    pub content_length: Option<i64>,
}

#[derive(Serialize)]
pub struct Drc20Token {
    pub tick: String,
    pub max_supply: String,
    pub minted: String,
    pub deployer: String,
    pub block_height: i64,
}

#[derive(Serialize)]
pub struct DunesToken {
    pub name: String,
    pub spaced_name: String,
    pub block: i64,
    pub mints: i64,
    pub burned: String,
    pub divisibility: i32,
}

#[derive(Serialize)]
pub struct LottoTicket {
    pub ticket_id: String,
    pub lotto_name: String,
    pub player_address: String,
    pub block_height: i64,
    pub tip_percent: i32,
}

#[derive(Serialize)]
pub struct LottoWinner {
    pub ticket_id: String,
    pub lotto_name: String,
    pub player_address: String,
    pub gross_payout_koinu: i64,
    pub tip_deduction_koinu: i64,
    pub draw_block: i64,
}

#[derive(Serialize)]
pub struct DnsName {
    pub name: String,
    pub inscription_id: String,
    pub block_height: i64,
    pub block_timestamp: i64,
}

#[derive(Serialize)]
pub struct DogemapClaim {
    pub block_number: i64,
    pub inscription_id: String,
    pub claim_height: i64,
    pub claim_timestamp: i64,
}

#[derive(Serialize)]
pub struct DogetagEntry {
    pub id: i64,
    pub txid: String,
    pub block_height: i64,
    pub block_timestamp: i64,
    pub sender_address: Option<String>,
    pub message: String,
    pub message_bytes: i32,
}

#[derive(Serialize)]
pub struct DogeSpellsBalanceEntry {
    pub ticker: String,
    pub address: String,
    pub balance: String,
}

#[derive(Serialize)]
pub struct DogeSpellsSpellEntry {
    pub txid: String,
    pub vout: u32,
    pub block_height: u64,
    pub block_timestamp: u32,
    pub version: String,
    pub tag: String,
    pub op: String,
    pub id: String,
    pub chain_id: String,
    pub ticker: Option<String>,
    pub name: Option<String>,
    pub amount: Option<u64>,
    pub decimals: Option<u8>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub beam_to: Option<String>,
    pub beam_proof: Option<String>,
    pub raw_cbor: String,
}

#[derive(Clone, Serialize)]
struct NodeTelemetry {
    connected: bool,
    block_height: Option<u64>,
    difficulty: Option<f64>,
    network_hashps: Option<f64>,
    mempool_size: usize,
    mempool_bytes: usize,
    blockchain_size: Option<u64>,
    verification_progress: Option<f64>,
    connections: Option<usize>,
    timestamp: i64,
    error: Option<String>,
}

fn unix_timestamp_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn normalize_timestamp_millis(timestamp: i64) -> i64 {
    if timestamp > 0 && timestamp < 1_000_000_000_000 {
        timestamp.saturating_mul(1000)
    } else {
        timestamp
    }
}

fn value_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

fn value_u64(payload: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(value) = payload.get(*key) {
            if let Some(parsed) = value.as_u64() {
                return Some(parsed);
            }
            if let Some(parsed) = value.as_i64() {
                if parsed >= 0 {
                    return Some(parsed as u64);
                }
            }
            if let Some(parsed) = value.as_str().and_then(|raw| raw.parse::<u64>().ok()) {
                return Some(parsed);
            }
        }
    }
    None
}

fn build_monitor_title(event_name: &str) -> String {
    match event_name {
        "dns.registered" => "DNS registration".to_string(),
        "dogemap.claimed" => "Dogemap claim".to_string(),
        "dogetag.tagged" => "Dogetag".to_string(),
        "dogelotto.ticket_minted" => "DogeLotto ticket".to_string(),
        "dogelotto.winner_resolved" => "DogeLotto draw".to_string(),
        "dogespells.mint" => "DogeSpells mint".to_string(),
        "dogespells.transfer" => "DogeSpells transfer".to_string(),
        "dogespells.burn" => "DogeSpells burn".to_string(),
        "dogespells.beam_out" => "DogeSpells beam out".to_string(),
        "dogespells.beam_in" => "DogeSpells beam in".to_string(),
        "dmp.listing" => "DMP listing".to_string(),
        "dmp.bid" => "DMP bid".to_string(),
        "dmp.settle" => "DMP settlement".to_string(),
        "dmp.cancel" => "DMP cancellation".to_string(),
        _ => "Indexed item".to_string(),
    }
}

fn build_monitor_summary(event_name: &str, payload: &Value) -> String {
    match event_name {
        "dns.registered" => format!(
            "{} registered",
            value_string(payload, "name").unwrap_or_else(|| "Unnamed DNS entry".to_string())
        ),
        "dogemap.claimed" => format!(
            "Block #{} claimed",
            value_u64(payload, &["block_number"])
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
        "dogetag.tagged" => {
            value_string(payload, "message").unwrap_or_else(|| "New on-chain Dogetag".to_string())
        }
        "dogelotto.ticket_minted" => format!(
            "Ticket {} minted for {}",
            value_string(payload, "ticket_id").unwrap_or_else(|| "unknown".to_string()),
            value_string(payload, "lotto_id").unwrap_or_else(|| "DogeLotto".to_string())
        ),
        "dogelotto.winner_resolved" => format!(
            "Winner {} resolved for {}",
            value_string(payload, "ticket_id").unwrap_or_else(|| "unknown".to_string()),
            value_string(payload, "lotto_id").unwrap_or_else(|| "DogeLotto".to_string())
        ),
        "dogespells.mint"
        | "dogespells.transfer"
        | "dogespells.burn"
        | "dogespells.beam_out"
        | "dogespells.beam_in" => format!(
            "{} {} {}",
            value_string(payload, "ticker").unwrap_or_else(|| "spell".to_string()),
            value_string(payload, "op").unwrap_or_else(|| "update".to_string()),
            value_u64(payload, &["amount"])
                .map(|value| value.to_string())
                .unwrap_or_else(|| "0".to_string())
        ),
        "dmp.listing" => format!(
            "Listing {} @ {} koinu",
            value_string(payload, "inscription_id").unwrap_or_else(|| "unknown".to_string()),
            value_u64(payload, &["price_koinu"])
                .map(|value| value.to_string())
                .unwrap_or_else(|| "0".to_string())
        ),
        "dmp.bid" => format!(
            "Bid on {} @ {} koinu",
            value_string(payload, "listing_id").unwrap_or_else(|| "unknown".to_string()),
            value_u64(payload, &["price_koinu"])
                .map(|value| value.to_string())
                .unwrap_or_else(|| "0".to_string())
        ),
        "dmp.settle" => format!(
            "Settled listing {}",
            value_string(payload, "listing_id").unwrap_or_else(|| "unknown".to_string())
        ),
        "dmp.cancel" => format!(
            "Cancelled listing {}",
            value_string(payload, "listing_id").unwrap_or_else(|| "unknown".to_string())
        ),
        _ => "Indexed item available immediately".to_string(),
    }
}

fn monitor_link(
    event_name: &str,
    txid: Option<&str>,
    inscription_id: Option<&str>,
) -> Option<String> {
    if let Some(inscription_id) = inscription_id {
        if !inscription_id.is_empty() {
            return Some(format!("/content/{}", inscription_id));
        }
    }
    if let Some(txid) = txid {
        if !txid.is_empty() {
            return Some(format!("https://dogechain.info/tx/{}", txid));
        }
    }
    if event_name == "dogemap.claimed" {
        return Some("https://dogemap.org".to_string());
    }
    None
}

fn normalize_monitor_event(payload: &Value) -> MonitorEvent {
    let event_name = value_string(payload, "event").unwrap_or_else(|| "indexed.item".to_string());
    let txid = value_string(payload, "tx_id")
        .or_else(|| value_string(payload, "txid"))
        .or_else(|| {
            value_string(payload, "inscription_id").map(|inscription_id| {
                inscription_id
                    .split('i')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
        });
    let inscription_id = value_string(payload, "inscription_id");
    let block_height = value_u64(
        payload,
        &[
            "block_height",
            "claim_height",
            "minted_height",
            "resolved_height",
        ],
    );
    let timestamp = unix_timestamp_millis();
    let link = monitor_link(&event_name, txid.as_deref(), inscription_id.as_deref());

    MonitorEvent {
        id: value_string(payload, "_id")
            .unwrap_or_else(|| format!("monitor-{}-{}", event_name, timestamp)),
        kind: event_name
            .split('.')
            .next()
            .unwrap_or("indexed")
            .to_string(),
        protocol: event_name.clone(),
        title: build_monitor_title(&event_name),
        summary: build_monitor_summary(&event_name, payload),
        link,
        txid,
        inscription_id,
        block_height,
        timestamp,
    }
}

fn record_monitor_event(state: &AppState, monitor_event: MonitorEvent) {
    state
        .monitor
        .webhook_deliveries
        .fetch_add(1, Ordering::Relaxed);

    if monitor_event.protocol.contains("reorg") {
        state.monitor.reorg_count.fetch_add(1, Ordering::Relaxed);
    }

    if let Some(height) = monitor_event.block_height {
        let mut samples = state
            .monitor
            .block_samples
            .lock()
            .expect("monitor block samples lock");
        let should_push = samples
            .back()
            .map(|sample| sample.height != height)
            .unwrap_or(true);
        if should_push {
            samples.push_back(MonitorBlockSample {
                height,
                timestamp: monitor_event.timestamp,
            });
            while samples.len() > super::MONITOR_SAMPLE_CAPACITY {
                samples.pop_front();
            }
        }
    }

    let mut events = state
        .monitor
        .recent_events
        .lock()
        .expect("monitor events lock");
    events.push_front(monitor_event);
    while events.len() > super::MONITOR_EVENT_CAPACITY {
        events.pop_back();
    }
}

pub async fn get_inscriptions(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<InscriptionRow>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT inscription_id, number AS inscription_number, block_height::bigint,
                    timestamp AS block_timestamp, content_type, content_length
             FROM inscriptions
             ORDER BY number DESC
             LIMIT $1 OFFSET $2",
            &[&params.limit, &params.offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let inscriptions: Vec<InscriptionRow> = rows
        .iter()
        .map(|row| InscriptionRow {
            inscription_id: row.get(0),
            inscription_number: row.get(1),
            block_height: row.get(2),
            block_timestamp: row.get(3),
            content_type: row.get(4),
            content_length: row.get(5),
        })
        .collect();

    Ok(Json(inscriptions))
}

pub async fn get_recent_inscriptions(
    State(state): State<AppState>,
) -> Result<Json<Vec<InscriptionRow>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT inscription_id, number AS inscription_number, block_height::bigint,
                    timestamp AS block_timestamp, content_type, content_length
             FROM inscriptions
             ORDER BY number DESC
             LIMIT 20",
            &[],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let inscriptions: Vec<InscriptionRow> = rows
        .iter()
        .map(|row| InscriptionRow {
            inscription_id: row.get(0),
            inscription_number: row.get(1),
            block_height: row.get(2),
            block_timestamp: row.get(3),
            content_type: row.get(4),
            content_length: row.get(5),
        })
        .collect();

    Ok(Json(inscriptions))
}

pub async fn get_drc20_tokens(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<Drc20Token>>, StatusCode> {
    let pool = state.drc20_pool.as_ref().ok_or(StatusCode::NOT_FOUND)?;

    let client = pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT ticker AS tick, max::text AS max_supply,
                    COALESCE(minted_supply, 0)::text AS minted,
                    address AS deployer, block_height::bigint
             FROM tokens
             ORDER BY block_height DESC
             LIMIT $1 OFFSET $2",
            &[&params.limit, &params.offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let tokens: Vec<Drc20Token> = rows
        .iter()
        .map(|row| Drc20Token {
            tick: row.get(0),
            max_supply: row.get(1),
            minted: row.get(2),
            deployer: row.get(3),
            block_height: row.get(4),
        })
        .collect();

    Ok(Json(tokens))
}

pub async fn get_dunes_tokens(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<DunesToken>>, StatusCode> {
    let pool = state.dunes_pool.as_ref().ok_or(StatusCode::NOT_FOUND)?;

    let client = pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT r.name, r.spaced_name, r.block_height::bigint AS block,
                    COALESCE(sc.total_mints, 0)::bigint AS mints,
                    COALESCE(sc.burned, 0)::text AS burned,
                    r.divisibility::int AS divisibility
             FROM dunes r
             LEFT JOIN LATERAL (
                 SELECT total_mints, burned
                 FROM supply_changes
                 WHERE dune_id = r.id
                 ORDER BY block_height DESC
                 LIMIT 1
             ) sc ON TRUE
             ORDER BY r.block_height DESC
             LIMIT $1 OFFSET $2",
            &[&params.limit, &params.offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let tokens: Vec<DunesToken> = rows
        .iter()
        .map(|row| DunesToken {
            name: row.get(0),
            spaced_name: row.get(1),
            block: row.get(2),
            mints: row.get(3),
            burned: row.get(4),
            divisibility: row.get(5),
        })
        .collect();

    Ok(Json(tokens))
}

pub async fn get_lotto_tickets(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<LottoTicket>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT ticket_id, lotto_id AS lotto_name,
                    inscription_id AS player_address,
                    minted_height AS block_height, tip_percent
             FROM dogelotto_tickets
             ORDER BY minted_height DESC
             LIMIT $1 OFFSET $2",
            &[&params.limit, &params.offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let tickets: Vec<LottoTicket> = rows
        .iter()
        .map(|row| LottoTicket {
            ticket_id: row.get(0),
            lotto_name: row.get(1),
            player_address: row.get(2),
            block_height: row.get(3),
            tip_percent: row.get(4),
        })
        .collect();

    Ok(Json(tickets))
}

pub async fn get_lotto_winners(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<LottoWinner>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT ticket_id, lotto_id AS lotto_name,
                    inscription_id AS player_address,
                    gross_payout_koinu, tip_deduction_koinu,
                    resolved_height AS draw_block
             FROM dogelotto_winners
             ORDER BY resolved_height DESC
             LIMIT $1 OFFSET $2",
            &[&params.limit, &params.offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let winners: Vec<LottoWinner> = rows
        .iter()
        .map(|row| LottoWinner {
            ticket_id: row.get(0),
            lotto_name: row.get(1),
            player_address: row.get(2),
            gross_payout_koinu: row.get(3),
            tip_deduction_koinu: row.get(4),
            draw_block: row.get(5),
        })
        .collect();

    Ok(Json(winners))
}

/// Query params for GET /api/dogelotto/verify
#[derive(Deserialize)]
pub struct LottoVerifyParams {
    /// Hex block hash of the draw block (the target u256).
    pub block_hash: String,
    /// Comma-separated seed numbers chosen on the ticket (e.g. "1,7,42,69,100,420").
    pub numbers: String,
    /// Optional: lotto_id to scope winner lookup. If omitted, only fingerprint data is returned.
    pub lotto_id: Option<String>,
}

/// Response for GET /api/dogelotto/verify
/// All computations are derived purely from Dogecoin chain data and are 100% verifiable.
#[derive(Serialize)]
pub struct LottoVerifyResponse {
    /// SHA256(sorted seed u16 pairs as big-endian bytes) = the ticket's fingerprint.
    pub fingerprint: String,
    /// block_hash interpreted as a big-endian u256.
    pub draw_target: String,
    /// Hex u256: |fingerprint − draw_target|. Smaller = closer = better rank.
    pub distance: String,
    /// Classic numbers (1-49) derived deterministically from the fingerprint.
    pub classic_numbers: Vec<u16>,
    /// Tie rule: tickets sharing the exact same distance split that tier equally.
    /// Display rank within a tie is sorted by inscription_id lex (smaller first).
    pub tie_rule: &'static str,
    /// If lotto_id was provided and the lottery is resolved: winner details, or null.
    pub winner: Option<serde_json::Value>,
}

pub async fn lotto_verify(
    State(state): State<AppState>,
    Query(params): Query<LottoVerifyParams>,
) -> Result<Json<LottoVerifyResponse>, StatusCode> {
    use doginals_indexer::core::meta_protocols::lotto::{
        compute_ticket_fingerprint, derive_classic_numbers, u256_abs_diff,
    };

    // Parse numbers
    let seed_numbers: Vec<u16> = params
        .numbers
        .split(',')
        .filter_map(|s| s.trim().parse::<u16>().ok())
        .collect();
    if seed_numbers.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let fp_bytes = compute_ticket_fingerprint(&seed_numbers);
    let fp_hex = hex::encode(fp_bytes);

    // Parse block hash as u256
    let hash_hex = params.block_hash.trim_start_matches("0x");
    let hash_bytes_vec = hex::decode(hash_hex).map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut draw_target = [0u8; 32];
    let copy_len = hash_bytes_vec.len().min(32);
    draw_target[..copy_len].copy_from_slice(&hash_bytes_vec[..copy_len]);

    let distance_bytes = u256_abs_diff(&fp_bytes, &draw_target);
    let distance_hex = hex::encode(distance_bytes);

    let classic_numbers = derive_classic_numbers(&fp_bytes);

    // Optionally look up winner record
    let winner = if let Some(ref lotto_id) = params.lotto_id {
        let client = state
            .doginals_pool
            .get()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        client
            .query_opt(
                "SELECT w.rank, w.payout_koinu, w.classic_matches, w.classic_payout_koinu,
                        w.fingerprint_distance, w.inscription_id
                 FROM dogelotto_winners w
                 WHERE w.lotto_id = $1 AND w.fingerprint_distance = $2",
                &[lotto_id, &distance_hex],
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map(|row| {
                json!({
                    "rank": row.get::<_, i32>("rank"),
                    "payout_koinu": row.get::<_, i64>("payout_koinu"),
                    "classic_matches": row.get::<_, i32>("classic_matches"),
                    "classic_payout_koinu": row.get::<_, i64>("classic_payout_koinu"),
                    "fingerprint_distance": row.get::<_, Option<String>>("fingerprint_distance"),
                    "inscription_id": row.get::<_, String>("inscription_id"),
                })
            })
    } else {
        None
    };

    Ok(Json(LottoVerifyResponse {
        fingerprint: fp_hex,
        draw_target: hex::encode(draw_target),
        distance: distance_hex,
        classic_numbers,
        tie_rule: "Tickets sharing the exact same |fingerprint - draw_target| distance split \
                   that prize tier equally. Within a tie group, display rank is sorted by \
                   inscription_id lexicographic (lex-smaller first) for display purposes only.",
        winner,
    }))
}

pub async fn get_dns_names(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<DnsName>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT name, inscription_id, block_height, block_timestamp
             FROM dns_names
             ORDER BY block_height DESC
             LIMIT $1 OFFSET $2",
            &[&params.limit, &params.offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let names: Vec<DnsName> = rows
        .iter()
        .map(|row| DnsName {
            name: row.get(0),
            inscription_id: row.get(1),
            block_height: row.get(2),
            block_timestamp: row.get(3),
        })
        .collect();

    Ok(Json(names))
}

pub async fn get_dogemap_claims(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<DogemapClaim>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT block_number, inscription_id, claim_height, claim_timestamp
             FROM dogemap_claims
             ORDER BY claim_height DESC
             LIMIT $1 OFFSET $2",
            &[&params.limit, &params.offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let claims: Vec<DogemapClaim> = rows
        .iter()
        .map(|row| DogemapClaim {
            block_number: row.get(0),
            inscription_id: row.get(1),
            claim_height: row.get(2),
            claim_timestamp: row.get(3),
        })
        .collect();

    Ok(Json(claims))
}

async fn fetch_node_telemetry(config: config::DogecoinConfig) -> NodeTelemetry {
    tokio::task::spawn_blocking(move || {
        let ctx = dogecoin::utils::Context::empty();
        let rpc = dogecoin::utils::dogecoind::dogecoin_get_client(&config, &ctx);
        let block_height = rpc.get_block_count().ok();
        let blockchain_info = rpc.get_blockchain_info().ok();
        let mempool_info = rpc.get_mempool_info().ok();
        let network_info = rpc.get_network_info().ok();
        let network_hashps = rpc.get_network_hash_ps(Some(120), None).ok();

        NodeTelemetry {
            connected: block_height.is_some(),
            block_height,
            difficulty: blockchain_info.as_ref().map(|info| info.difficulty),
            network_hashps,
            mempool_size: mempool_info
                .as_ref()
                .map(|info| info.size)
                .unwrap_or_default(),
            mempool_bytes: mempool_info
                .as_ref()
                .map(|info| info.bytes)
                .unwrap_or_default(),
            blockchain_size: blockchain_info.as_ref().map(|info| info.size_on_disk),
            verification_progress: blockchain_info
                .as_ref()
                .map(|info| info.verification_progress),
            connections: network_info.as_ref().map(|info| info.connections),
            timestamp: unix_timestamp_millis(),
            error: if block_height.is_some() {
                None
            } else {
                Some("Unable to reach Dogecoin RPC".to_string())
            },
        }
    })
    .await
    .unwrap_or_else(|error| NodeTelemetry {
        connected: false,
        block_height: None,
        difficulty: None,
        network_hashps: None,
        mempool_size: 0,
        mempool_bytes: 0,
        blockchain_size: None,
        verification_progress: None,
        connections: None,
        timestamp: unix_timestamp_millis(),
        error: Some(error.to_string()),
    })
}

fn compute_blocks_per_second(samples: &[MonitorBlockSample]) -> f64 {
    if samples.len() < 2 {
        return 0.0;
    }

    let first = &samples[0];
    let last = &samples[samples.len() - 1];
    if last.timestamp <= first.timestamp || last.height <= first.height {
        return 0.0;
    }

    (last.height - first.height) as f64 / ((last.timestamp - first.timestamp) as f64 / 1000.0)
}

fn compute_items_per_second(events: &[MonitorEvent]) -> f64 {
    let now = unix_timestamp_millis();
    let recent = events
        .iter()
        .filter(|event| {
            now.saturating_sub(event.timestamp) <= 60_000
                && (event.kind == "inscription" || event.inscription_id.is_some())
        })
        .count();
    recent as f64 / 60.0
}

pub async fn get_status(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let summary_row = client
        .query_one(
            "SELECT
                COALESCE((SELECT COUNT(*)::bigint FROM inscriptions), 0) AS total_inscriptions,
                COALESCE((SELECT COUNT(*)::bigint FROM dogespells), 0) AS total_dogespells,
                COALESCE((SELECT COUNT(*)::bigint FROM dns_names), 0) AS total_dns,
                COALESCE((SELECT COUNT(*)::bigint FROM dogemap_claims), 0) AS total_dogemap,
                COALESCE((SELECT COUNT(*)::bigint FROM dogetags), 0) AS total_dogetags,
                COALESCE((SELECT COUNT(*)::bigint FROM dmp_listings), 0)
                  + COALESCE((SELECT COUNT(*)::bigint FROM dmp_bids), 0)
                  + COALESCE((SELECT COUNT(*)::bigint FROM dmp_settlements), 0)
                  + COALESCE((SELECT COUNT(*)::bigint FROM dmp_cancels), 0) AS total_dmp,
                COALESCE((SELECT COUNT(*)::bigint FROM dogelotto_tickets), 0)
                  + COALESCE((SELECT COUNT(*)::bigint FROM dogelotto_winners), 0)
                  + COALESCE((SELECT COUNT(*)::bigint FROM dogelotto_lotteries), 0) AS total_dogelotto,
                GREATEST(
                  COALESCE((SELECT MAX(block_height)::bigint FROM inscriptions), 0),
                  COALESCE((SELECT MAX(block_height)::bigint FROM dogespells), 0),
                  COALESCE((SELECT MAX(block_height)::bigint FROM dmp_listings), 0),
                  COALESCE((SELECT MAX(block_height)::bigint FROM dmp_bids), 0),
                  COALESCE((SELECT MAX(block_height)::bigint FROM dmp_settlements), 0),
                  COALESCE((SELECT MAX(block_height)::bigint FROM dmp_cancels), 0),
                  COALESCE((SELECT MAX(minted_height)::bigint FROM dogelotto_tickets), 0),
                  COALESCE((SELECT MAX(resolved_height)::bigint FROM dogelotto_winners), 0),
                  COALESCE((SELECT MAX(deploy_height)::bigint FROM dogelotto_lotteries), 0),
                  COALESCE((SELECT MAX(block_height)::bigint FROM dogetags), 0),
                  COALESCE((SELECT MAX(claim_height)::bigint FROM dogemap_claims), 0),
                  COALESCE((SELECT MAX(block_height)::bigint FROM dns_names), 0)
                ) AS latest_indexed_block,
                (SELECT MAX(timestamp)::bigint FROM inscriptions) AS latest_block_timestamp",
            &[],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let latest_indexed_block: i64 = summary_row.get("latest_indexed_block");
    let latest_block_timestamp: Option<i64> = summary_row.get("latest_block_timestamp");
    let total_inscriptions = summary_row.get::<_, i64>("total_inscriptions");
    let total_dogespells = summary_row.get::<_, i64>("total_dogespells");
    let total_dns = summary_row.get::<_, i64>("total_dns");
    let total_dogemap = summary_row.get::<_, i64>("total_dogemap");
    let total_dogetags = summary_row.get::<_, i64>("total_dogetags");
    let total_dmp = summary_row.get::<_, i64>("total_dmp");
    let total_dogelotto = summary_row.get::<_, i64>("total_dogelotto");
    let total_indexed = total_inscriptions
        + total_dogespells
        + total_dns
        + total_dogemap
        + total_dogetags
        + total_dmp
        + total_dogelotto;
    let node = fetch_node_telemetry(state.dogecoin_config.clone()).await;
    let chain_tip = node.block_height;
    let sync_progress = chain_tip.map(|tip| {
        if tip == 0 {
            0.0
        } else {
            latest_indexed_block as f64 / tip as f64
        }
    });
    let sample_snapshot: Vec<MonitorBlockSample> = state
        .monitor
        .block_samples
        .lock()
        .expect("monitor block samples lock")
        .iter()
        .cloned()
        .collect();
    let recent_events: Vec<MonitorEvent> = state
        .monitor
        .recent_events
        .lock()
        .expect("monitor events lock")
        .iter()
        .cloned()
        .collect();
    let buffered_events = recent_events.len();

    Ok(Json(json!({
        "status": "running",
        "connected": node.connected,
        "error": node.error,
        "timestamp": node.timestamp,
        "total_indexed": total_indexed,
        "total_inscriptions": total_inscriptions,
        "latest_indexed_block": latest_indexed_block,
        "latest_block_timestamp": latest_block_timestamp,
        "chain_tip": chain_tip,
        "sync_progress": sync_progress,
        "blocks_per_second": compute_blocks_per_second(&sample_snapshot),
        "inscriptions_per_second": compute_items_per_second(&recent_events),
        "dogespells_count": total_dogespells,
        "dns_count": total_dns,
        "dogemap_count": total_dogemap,
        "dogetag_count": total_dogetags,
        "dmp_count": total_dmp,
        "dogelotto_count": total_dogelotto,
        "difficulty": node.difficulty,
        "network_hashps": node.network_hashps,
        "mempool_size": node.mempool_size,
        "mempool_bytes": node.mempool_bytes,
        "blockchain_size": node.blockchain_size,
        "verification_progress": node.verification_progress,
        "connections": node.connections,
        "buffered_events": buffered_events,
        "webhook_deliveries": state.monitor.webhook_deliveries.load(Ordering::Relaxed),
        "duplicate_deliveries": state.monitor.duplicate_deliveries.load(Ordering::Relaxed),
        "reorg_count": state.monitor.reorg_count.load(Ordering::Relaxed),
        "additive_viewing": true,
        "partial_results": true,
    })))
}

pub async fn get_monitor(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let summary_value = get_status(State(state.clone())).await?.0;

    let recent_inscriptions = client
        .query(
            "SELECT inscription_id, number::bigint, block_height::bigint, timestamp, content_type
             FROM inscriptions
             ORDER BY number DESC
             LIMIT 12",
            &[],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut feed: Vec<MonitorEvent> = state
        .monitor
        .recent_events
        .lock()
        .expect("monitor events lock")
        .iter()
        .cloned()
        .collect();

    for row in recent_inscriptions {
        let inscription_id: String = row.get(0);
        if feed
            .iter()
            .any(|event| event.inscription_id.as_deref() == Some(inscription_id.as_str()))
        {
            continue;
        }

        let inscription_number: i64 = row.get(1);
        let block_height: i64 = row.get(2);
        let block_timestamp: i64 = row.get(3);
        let content_type: Option<String> = row.get(4);

        feed.push(MonitorEvent {
            id: format!("inscription-{}", inscription_id),
            kind: "inscription".to_string(),
            protocol: "inscription.indexed".to_string(),
            title: "Inscription indexed".to_string(),
            summary: format!(
                "#{} {}",
                inscription_number,
                content_type.unwrap_or_else(|| "content".to_string())
            ),
            link: Some(format!("/content/{}", inscription_id)),
            txid: inscription_id
                .split('i')
                .next()
                .map(|value| value.to_string()),
            inscription_id: Some(inscription_id),
            block_height: Some(block_height as u64),
            timestamp: normalize_timestamp_millis(block_timestamp),
        });
    }

    feed.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    feed.truncate(40);

    let total_indexed = summary_value
        .get("total_indexed")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let buffered_events = feed.len();

    let buffer_memory_bytes = serde_json::to_string(&feed)
        .unwrap_or_default()
        .as_bytes()
        .len() as u64;
    let blocks_per_second = {
        let samples: Vec<MonitorBlockSample> = state
            .monitor
            .block_samples
            .lock()
            .expect("monitor block samples lock")
            .iter()
            .cloned()
            .collect();
        compute_blocks_per_second(&samples)
    };

    Ok(Json(json!({
        "status": summary_value,
        "stats": {
            "total_indexed": total_indexed,
            "memory_usage_bytes": buffer_memory_bytes,
            "memory_usage_mb": buffer_memory_bytes as f64 / 1024.0 / 1024.0,
            "reorg_count": state.monitor.reorg_count.load(Ordering::Relaxed),
            "webhook_deliveries": state.monitor.webhook_deliveries.load(Ordering::Relaxed),
            "duplicate_deliveries": state.monitor.duplicate_deliveries.load(Ordering::Relaxed),
            "blocks_per_second": blocks_per_second,
            "buffered_events": buffered_events,
        },
        "protocol_counts": {
            "inscriptions": summary_value.get("total_inscriptions").cloned().unwrap_or(Value::Null),
            "dogespells": summary_value.get("dogespells_count").cloned().unwrap_or(Value::Null),
            "dmp": summary_value.get("dmp_count").cloned().unwrap_or(Value::Null),
            "dogelotto": summary_value.get("dogelotto_count").cloned().unwrap_or(Value::Null),
            "dns": summary_value.get("dns_count").cloned().unwrap_or(Value::Null),
            "dogemap": summary_value.get("dogemap_count").cloned().unwrap_or(Value::Null),
            "dogetag": summary_value.get("dogetag_count").cloned().unwrap_or(Value::Null),
        },
        "node": {
            "connected": summary_value.get("connected").cloned().unwrap_or(Value::Bool(false)),
            "block_height": summary_value.get("chain_tip").cloned().unwrap_or(Value::Null),
            "difficulty": summary_value.get("difficulty").cloned().unwrap_or(Value::Null),
            "network_hashps": summary_value.get("network_hashps").cloned().unwrap_or(Value::Null),
            "mempool_size": summary_value.get("mempool_size").cloned().unwrap_or(Value::Null),
            "mempool_bytes": summary_value.get("mempool_bytes").cloned().unwrap_or(Value::Null),
            "blockchain_size": summary_value.get("blockchain_size").cloned().unwrap_or(Value::Null),
            "verification_progress": summary_value
                .get("verification_progress")
                .cloned()
                .unwrap_or(Value::Null),
            "connections": summary_value.get("connections").cloned().unwrap_or(Value::Null),
            "timestamp": summary_value.get("timestamp").cloned().unwrap_or(Value::Null),
            "error": summary_value.get("error").cloned().unwrap_or(Value::Null),
        },
        "feed": feed,
        "immediate_additive_viewing": true,
    })))
}

pub async fn get_dogetags(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<DogetagEntry>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT id, txid, block_height::bigint, block_timestamp,
                    sender_address, message, message_bytes
             FROM dogetags
             ORDER BY block_height DESC, id DESC
             LIMIT $1 OFFSET $2",
            &[&params.limit, &params.offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let tags: Vec<DogetagEntry> = rows
        .iter()
        .map(|row| DogetagEntry {
            id: row.get(0),
            txid: row.get(1),
            block_height: row.get(2),
            block_timestamp: row.get(3),
            sender_address: row.get(4),
            message: row.get(5),
            message_bytes: row.get(6),
        })
        .collect();

    Ok(Json(tags))
}

pub async fn get_dogespells_balance(
    State(state): State<AppState>,
    Path((ticker, address)): Path<(String, String)>,
) -> Result<Json<DogeSpellsBalanceEntry>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let balance = client
        .query_opt(
            "SELECT balance::text
             FROM dogespells_balances
             WHERE ticker = $1 AND address = $2",
            &[&ticker, &address],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|row| row.get::<_, String>(0))
        .unwrap_or_else(|| "0".to_string());

    Ok(Json(DogeSpellsBalanceEntry {
        ticker,
        address,
        balance,
    }))
}

pub async fn get_dogespells_history(
    State(state): State<AppState>,
    Path((ticker, address)): Path<(String, String)>,
) -> Result<Json<Vec<DogeSpellsSpellEntry>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT txid, vout, block_height::bigint, block_timestamp, version, tag, op,
                    identity, chain_id, ticker, name, amount::text, decimals::int,
                    from_addr, to_addr, beam_to, beam_proof, raw_cbor
             FROM dogespells
             WHERE ticker = $1
               AND (from_addr = $2 OR to_addr = $2)
             ORDER BY block_height DESC, id DESC",
            &[&ticker, &address],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let spells: Vec<DogeSpellsSpellEntry> = rows
        .iter()
        .map(|row| DogeSpellsSpellEntry {
            txid: row.get(0),
            vout: row.get::<_, i64>(1) as u32,
            block_height: row.get::<_, i64>(2) as u64,
            block_timestamp: row.get::<_, i64>(3) as u32,
            version: row.get(4),
            tag: row.get(5),
            op: row.get(6),
            id: row.get(7),
            chain_id: row.get(8),
            ticker: row.get(9),
            name: row.get(10),
            amount: row
                .get::<_, Option<String>>(11)
                .and_then(|value| value.parse::<u64>().ok()),
            decimals: row.get::<_, Option<i32>>(12).map(|value| value as u8),
            from: row.get(13),
            to: row.get(14),
            beam_to: row.get(15),
            beam_proof: row.get(16),
            raw_cbor: hex::encode(row.get::<_, Vec<u8>>(17)),
        })
        .collect();

    Ok(Json(spells))
}

pub async fn get_dogespells_spells(
    State(state): State<AppState>,
    Path(txid): Path<String>,
) -> Result<Json<Vec<DogeSpellsSpellEntry>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT txid, vout, block_height::bigint, block_timestamp, version, tag, op,
                    identity, chain_id, ticker, name, amount::text, decimals::int,
                    from_addr, to_addr, beam_to, beam_proof, raw_cbor
             FROM dogespells
             WHERE LOWER(txid) = LOWER($1)
             ORDER BY vout ASC, id ASC",
            &[&txid],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let spells: Vec<DogeSpellsSpellEntry> = rows
        .iter()
        .map(|row| DogeSpellsSpellEntry {
            txid: row.get(0),
            vout: row.get::<_, i64>(1) as u32,
            block_height: row.get::<_, i64>(2) as u64,
            block_timestamp: row.get::<_, i64>(3) as u32,
            version: row.get(4),
            tag: row.get(5),
            op: row.get(6),
            id: row.get(7),
            chain_id: row.get(8),
            ticker: row.get(9),
            name: row.get(10),
            amount: row
                .get::<_, Option<String>>(11)
                .and_then(|value| value.parse::<u64>().ok()),
            decimals: row.get::<_, Option<i32>>(12).map(|value| value as u8),
            from: row.get(13),
            to: row.get(14),
            beam_to: row.get(15),
            beam_proof: row.get(16),
            raw_cbor: hex::encode(row.get::<_, Vec<u8>>(17)),
        })
        .collect();

    Ok(Json(spells))
}

// ---------------------------------------------------------------------------
// Inscription decode — no index required, hits Dogecoin Core RPC directly
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DecodeParams {
    pub inscription_id: Option<String>,
    pub txid: Option<String>,
}

fn txid_from_inscription_id(iid: &str) -> String {
    match iid.rfind('i') {
        Some(pos) => iid[..pos].to_string(),
        None => iid.to_string(),
    }
}

fn parse_envelope_index(inscription_id: &str) -> usize {
    inscription_id
        .rfind('i')
        .and_then(|pos| inscription_id[pos + 1..].parse::<usize>().ok())
        .unwrap_or(0)
}

fn fetch_envelopes(
    dogecoin_config: &config::DogecoinConfig,
    txid_str: &str,
) -> Result<(Vec<ParsedEnvelope>, String), String> {
    let ctx = dogecoin::utils::Context::empty();
    let rpc = dogecoin::utils::dogecoind::dogecoin_get_client(dogecoin_config, &ctx);
    let txid: dogecoin::bitcoincore_rpc::bitcoin::Txid = txid_str
        .parse()
        .map_err(|e| format!("Invalid txid '{}': {}", txid_str, e))?;
    let raw_hex = rpc
        .get_raw_transaction_hex(&txid, None)
        .map_err(|e| format!("getrawtransaction {}: {}", txid_str, e))?;
    let raw_bytes = hex::decode(&raw_hex).map_err(|e| format!("hex decode error: {}", e))?;
    let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&raw_bytes)
        .map_err(|e| format!("tx deserialize error: {}", e))?;
    let envelopes = ParsedEnvelope::from_transactions_dogecoin(&[tx]);
    Ok((envelopes, txid_str.to_string()))
}

pub async fn decode_inscription(
    State(state): State<AppState>,
    Query(params): Query<DecodeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (raw_id, envelope_index) = if let Some(iid) = &params.inscription_id {
        (txid_from_inscription_id(iid), parse_envelope_index(iid))
    } else if let Some(t) = &params.txid {
        (t.clone(), 0)
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provide inscription_id or txid".to_string(),
        ));
    };

    let config = state.dogecoin_config.clone();
    let txid_str = raw_id.clone();
    let (envelopes, _) = tokio::task::spawn_blocking(move || fetch_envelopes(&config, &txid_str))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    if envelopes.is_empty() {
        return Ok(Json(json!({
            "found": false,
            "inscription_id": format!("{}i0", raw_id),
            "error": "No inscriptions found in this transaction"
        })));
    }

    let env = envelopes.get(envelope_index).unwrap_or(&envelopes[0]);
    let insc = &env.payload;
    let content_type = insc
        .content_type
        .as_ref()
        .and_then(|ct| std::str::from_utf8(ct).ok())
        .map(str::to_string);
    let metaprotocol = insc
        .metaprotocol
        .as_ref()
        .and_then(|mp| std::str::from_utf8(mp).ok())
        .map(str::to_string);
    let content_length = insc.body.as_ref().map(|b| b.len());
    let ct = content_type.as_deref().unwrap_or("");
    let body_text = if ct.starts_with("text/") || ct == "application/json" {
        insc.body
            .as_ref()
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(str::to_string)
    } else {
        None
    };
    let has_content = insc.body.as_ref().map(|b| !b.is_empty()).unwrap_or(false);

    Ok(Json(json!({
        "found": true,
        "inscription_id": format!("{}i{}", raw_id, envelope_index),
        "content_type": content_type,
        "content_length": content_length,
        "metaprotocol": metaprotocol,
        "has_content": has_content,
        "body_text": body_text,
        "content_url": format!("/content/{}i{}", raw_id, envelope_index),
    })))
}

pub async fn get_inscription_content(
    State(state): State<AppState>,
    Path(inscription_id): Path<String>,
) -> Response {
    let txid_str = txid_from_inscription_id(&inscription_id);
    let envelope_index = parse_envelope_index(&inscription_id);
    let config = state.dogecoin_config.clone();
    let txid_clone = txid_str.clone();

    let result = tokio::task::spawn_blocking(move || fetch_envelopes(&config, &txid_clone)).await;

    let envelopes = match result {
        Ok(Ok((e, _))) => e,
        Ok(Err(e)) => {
            return (StatusCode::BAD_REQUEST, e).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    let env = match envelopes.get(envelope_index) {
        Some(e) => e,
        None => {
            return (StatusCode::NOT_FOUND, "Inscription not found").into_response();
        }
    };

    let insc = &env.payload;
    let content_type = insc
        .content_type
        .as_ref()
        .and_then(|ct| std::str::from_utf8(ct).ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let body = insc.body.clone().unwrap_or_default();

    ([(header::CONTENT_TYPE, content_type)], Bytes::from(body)).into_response()
}

pub async fn index_page() -> Html<&'static str> {
    Html(include_str!("../../static/index.html"))
}

/// GET /api/events — SSE stream of indexer events.
///
/// Clients subscribe once and receive a real-time stream of JSON event objects
/// (same payloads that webhooks deliver). A 30-second keepalive comment is sent
/// so proxies and browsers don't close idle connections.
///
/// Example client (JS):
///   const es = new EventSource('https://api.wzrd.dog/api/events');
///   es.onmessage = e => console.log(JSON.parse(e.data));
pub async fn sse_events(
    State(state): State<super::AppState>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(json) => Some(Ok(Event::default().data(json))),
        // BroadcastStream::Lagged — subscriber was too slow, skip missed events
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// POST /api/webhook — receives indexer webhook payloads and fans them out to SSE subscribers.
///
/// kabosu automatically registers http://127.0.0.1:{port}/api/webhook as a webhook
/// URL at startup, so no manual config is needed.
pub async fn receive_webhook(State(state): State<super::AppState>, body: String) -> StatusCode {
    if let Ok(payload) = serde_json::from_str::<Value>(&body) {
        if let Some(delivery_id) = value_string(&payload, "_id") {
            let mut seen = state
                .monitor
                .seen_delivery_ids
                .lock()
                .expect("monitor seen delivery ids lock");

            if seen.iter().any(|existing| existing == &delivery_id) {
                state
                    .monitor
                    .duplicate_deliveries
                    .fetch_add(1, Ordering::Relaxed);
                return StatusCode::OK;
            }

            seen.push_back(delivery_id);
            while seen.len() > super::MONITOR_SEEN_ID_CAPACITY {
                seen.pop_front();
            }
        }

        let monitor_event = normalize_monitor_event(&payload);
        record_monitor_event(&state, monitor_event);
    }

    // Ignore send errors — they just mean no SSE clients are connected right now.
    let _ = state.event_tx.send(body);
    StatusCode::OK
}

pub async fn wallet_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../../static/wallet.js"),
    )
}

pub async fn inscriptions_page() -> Html<&'static str> {
    Html(include_str!("../../static/index.html"))
}

pub async fn drc20_page() -> Html<&'static str> {
    Html(include_str!("../../static/index.html"))
}

pub async fn dunes_page() -> Html<&'static str> {
    Html(include_str!("../../static/index.html"))
}

pub async fn lotto_page() -> Html<&'static str> {
    Html(include_str!("../../static/index.html"))
}

pub async fn koinu_relics_page() -> Redirect {
    Redirect::to("/static/koinu-relic-auto-theme.html")
}

pub async fn koinu_relic_template() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        KOINU_RELIC_TEMPLATE,
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceListResponse<T> {
    items: Vec<T>,
    total: usize,
    next_cursor: Option<String>,
}

static MARKETPLACE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceFeedParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    pub cursor: Option<String>,
    pub status: Option<String>,
    pub collection_id: Option<String>,
    pub seller_address: Option<String>,
    pub maker_address: Option<String>,
    pub inscription_id: Option<String>,
    pub min_price: Option<String>,
    pub max_price: Option<String>,
    pub sort: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMarketplaceListingRequest {
    pub inscription_id: String,
    pub collection_id: Option<String>,
    pub seller_address: String,
    pub asking_price_koinu: String,
    pub marketplace_fee_bps: Option<i32>,
    pub royalty_bps: Option<i32>,
    pub expiry_at: Option<String>,
    pub seller_signed_template: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelMarketplaceListingRequest {
    pub seller_address: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMarketplaceTraderRequest {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMarketplaceOfferRequest {
    pub scope: String,
    pub inscription_id: Option<String>,
    pub collection_id: Option<String>,
    pub maker_address: String,
    pub target_seller_address: Option<String>,
    pub offer_price_koinu: String,
    pub marketplace_fee_bps: Option<i32>,
    pub expires_at: String,
    pub intent_payload: Option<serde_json::Value>,
    pub signed_intent: MarketplaceSignedIntentEnvelope,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelMarketplaceOfferRequest {
    pub maker_address: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMarketplaceAuctionRequest {
    pub inscription_id: String,
    pub seller_address: String,
    pub start_price_koinu: String,
    pub reserve_price_koinu: Option<String>,
    pub min_increment_koinu: String,
    pub starts_at: String,
    pub ends_at: String,
    pub anti_sniping_window_sec: Option<i32>,
    pub anti_sniping_extension_sec: Option<i32>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceSignedIntentEnvelope {
    pub payload: serde_json::Value,
    pub signature: String,
    pub signing_address: String,
    pub signed_at: String,
    pub payload_hash: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceAuthChallengeRequest {
    pub address: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceAuthVerifyRequest {
    pub address: String,
    pub challenge_id: String,
    pub signature: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyMarketplaceXRequest {
    pub x_handle: String,
    pub x_user_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildMarketplaceOrderRequest {
    pub signed_intent: MarketplaceSignedIntentEnvelope,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitMarketplaceOrderRequest {
    pub signed_psbt: String,
    pub signed_intent: MarketplaceSignedIntentEnvelope,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMarketplaceBidRequest {
    pub bidder_address: String,
    pub bid_amount_koinu: String,
    pub signed_intent: MarketplaceSignedIntentEnvelope,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelMarketplaceBidRequest {
    pub bidder_address: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleMarketplaceAuctionRequest {
    pub seller_address: String,
    pub signed_psbt: String,
    pub signed_intent: MarketplaceSignedIntentEnvelope,
}

fn marketplace_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "code": code,
            "message": message.into(),
        })),
    )
        .into_response()
}

fn marketplace_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn marketplace_id(prefix: &str) -> String {
    let counter = MARKETPLACE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}_{}_{}",
        prefix,
        chrono::Utc::now().timestamp_millis(),
        counter
    )
}

fn marketplace_offset(params: &MarketplaceFeedParams) -> i64 {
    params
        .cursor
        .as_ref()
        .and_then(|cursor| cursor.parse::<i64>().ok())
        .unwrap_or(params.offset)
        .max(0)
}

fn marketplace_next_cursor(offset: i64, limit: i64, total: i64) -> Option<String> {
    let next = offset + limit.max(0);
    if next < total {
        Some(next.to_string())
    } else {
        None
    }
}

fn marketplace_parse_koinu(value: &str, field: &str) -> Result<i64, Response> {
    let parsed = value.parse::<i64>().map_err(|_| {
        marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_koinu",
            format!("{} must be an integer koinu amount", field),
        )
    })?;

    if parsed <= 0 {
        return Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_koinu",
            format!("{} must be greater than zero", field),
        ));
    }

    Ok(parsed)
}

fn marketplace_parse_optional_koinu(
    value: &Option<String>,
    field: &str,
) -> Result<Option<i64>, Response> {
    value
        .as_ref()
        .map(|value| marketplace_parse_koinu(value, field))
        .transpose()
}

fn marketplace_parse_timestamp(
    value: &str,
    field: &str,
) -> Result<chrono::DateTime<chrono::Utc>, Response> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
        .map_err(|_| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_timestamp",
                format!("{} must be a valid RFC3339 timestamp", field),
            )
        })
}

fn marketplace_network_name(network: bitcoin::Network) -> &'static str {
    match network {
        bitcoin::Network::Bitcoin => "mainnet",
        bitcoin::Network::Testnet => "testnet",
        bitcoin::Network::Regtest => "regtest",
        _ => "mainnet",
    }
}

fn marketplace_chain_id(network: bitcoin::Network) -> &'static str {
    match network {
        bitcoin::Network::Bitcoin => "doge-mainnet",
        bitcoin::Network::Testnet => "doge-testnet",
        bitcoin::Network::Regtest => "doge-regtest",
        _ => "doge-mainnet",
    }
}

fn marketplace_random_token(prefix: &str) -> String {
    let mut bytes = [0u8; 24];
    OsRng.fill_bytes(&mut bytes);
    format!("{}_{}", prefix, hex::encode(bytes))
}

fn marketplace_canonicalize_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(marketplace_canonicalize_json).collect())
        }
        serde_json::Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in map {
                sorted.insert(key.clone(), marketplace_canonicalize_json(value));
            }

            let mut normalized = serde_json::Map::new();
            for (key, value) in sorted {
                normalized.insert(key, value);
            }
            serde_json::Value::Object(normalized)
        }
        _ => value.clone(),
    }
}

fn marketplace_parse_dogecoin_address(addr: &str) -> Result<ScriptBuf, Response> {
    let decoded = base58::decode_check(addr).map_err(|_| {
        marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_address",
            format!("Invalid Dogecoin address '{}'", addr),
        )
    })?;

    if decoded.is_empty() {
        return Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_address",
            format!("Invalid Dogecoin address '{}'", addr),
        ));
    }

    let version = decoded[0];
    let payload = &decoded[1..];
    match version {
        0x1e => {
            let hash = PubkeyHash::from_slice(payload).map_err(|_| {
                marketplace_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_address",
                    format!("Invalid Dogecoin P2PKH address '{}'", addr),
                )
            })?;
            Ok(ScriptBuf::new_p2pkh(&hash))
        }
        0x16 => {
            let hash = ScriptHash::from_slice(payload).map_err(|_| {
                marketplace_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_address",
                    format!("Invalid Dogecoin P2SH address '{}'", addr),
                )
            })?;
            Ok(ScriptBuf::new_p2sh(&hash))
        }
        _ => Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_address",
            format!("Unsupported Dogecoin address version for '{}'", addr),
        )),
    }
}

fn marketplace_idempotency_key(headers: &HeaderMap) -> Result<String, Response> {
    headers
        .get("x-idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "missing_idempotency_key",
                "X-Idempotency-Key header is required",
            )
        })
}

fn marketplace_metadata_text(value: Option<&serde_json::Value>) -> Option<String> {
    value.and_then(|value| serde_json::to_string(value).ok())
}

fn marketplace_parse_metadata(value: Option<String>) -> serde_json::Value {
    value
        .as_deref()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .unwrap_or_else(|| json!({}))
}

fn marketplace_session_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get("x-marketplace-session")
        .and_then(|value| value.to_str().ok())
    {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

async fn marketplace_require_session(
    client: &deadpool_postgres::Client,
    headers: &HeaderMap,
    address: &str,
) -> Result<(), Response> {
    let token = marketplace_session_token(headers).ok_or_else(|| {
        marketplace_error(
            StatusCode::UNAUTHORIZED,
            "missing_session",
            "Marketplace session token is required",
        )
    })?;

    let row = client
        .query_opt(
            "SELECT expires_at, revoked_at
             FROM marketplace_sessions
             WHERE token = $1 AND address = $2",
            &[&token, &address],
        )
        .await
        .map_err(|_| {
            marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to verify marketplace session",
            )
        })?
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::UNAUTHORIZED,
                "invalid_session",
                "Marketplace session is invalid",
            )
        })?;

    let revoked_at: Option<String> = row.get("revoked_at");
    if revoked_at.is_some() {
        return Err(marketplace_error(
            StatusCode::UNAUTHORIZED,
            "invalid_session",
            "Marketplace session has been revoked",
        ));
    }

    let expires_at: String = row.get("expires_at");
    let expires_at = marketplace_parse_timestamp(&expires_at, "expiresAt")?;
    if expires_at <= chrono::Utc::now() {
        return Err(marketplace_error(
            StatusCode::UNAUTHORIZED,
            "expired_session",
            "Marketplace session has expired",
        ));
    }

    let now = marketplace_now();
    let _ = client
        .execute(
            "UPDATE marketplace_sessions
             SET last_used_at = $2
             WHERE token = $1",
            &[&token, &now],
        )
        .await;

    Ok(())
}

async fn marketplace_verify_message_signature(
    state: &AppState,
    address: &str,
    message: &str,
    signature: &str,
) -> Result<(), Response> {
    let ctx = dogecoin::utils::Context::empty();
    let rpc = dogecoin::utils::dogecoind::dogecoin_get_client(&state.dogecoin_config, &ctx);
    let verified = rpc
        .call::<bool>(
            "verifymessage",
            &[
                serde_json::to_value(address).unwrap_or(serde_json::Value::Null),
                serde_json::to_value(signature).unwrap_or(serde_json::Value::Null),
                serde_json::to_value(message).unwrap_or(serde_json::Value::Null),
            ],
        )
        .map_err(|_| {
            marketplace_error(
                StatusCode::BAD_GATEWAY,
                "rpc_error",
                "Failed to verify Dogecoin message signature",
            )
        })?;

    if !verified {
        return Err(marketplace_error(
            StatusCode::UNAUTHORIZED,
            "invalid_signature",
            "Dogecoin message signature verification failed",
        ));
    }

    Ok(())
}

async fn marketplace_consume_intent_nonce(
    client: &deadpool_postgres::Client,
    payload: &serde_json::Value,
    payload_hash: &str,
) -> Result<(), Response> {
    let nonce = payload
        .get("nonce")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_intent",
                "Intent nonce is required",
            )
        })?;
    let address = payload
        .get("address")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_intent",
                "Intent address is required",
            )
        })?;
    let intent_type = payload
        .get("intentType")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_intent",
                "Intent type is required",
            )
        })?;
    let expires_at = payload
        .get("expiresAt")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_intent",
                "Intent expiry is required",
            )
        })?;

    let now = marketplace_now();
    let inserted = client
        .execute(
            "INSERT INTO marketplace_intent_nonces (
                 nonce, address, intent_type, payload_hash, expires_at, consumed_at
             ) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (nonce) DO NOTHING",
            &[
                &nonce,
                &address,
                &intent_type,
                &payload_hash,
                &expires_at,
                &now,
            ],
        )
        .await
        .map_err(|_| {
            marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to record marketplace intent nonce",
            )
        })?;

    if inserted == 0 {
        return Err(marketplace_error(
            StatusCode::CONFLICT,
            "replayed_intent",
            "Marketplace intent nonce has already been used",
        ));
    }

    Ok(())
}

async fn marketplace_verify_signed_intent(
    state: &AppState,
    client: &deadpool_postgres::Client,
    envelope: &MarketplaceSignedIntentEnvelope,
    expected_intent_type: &str,
    expected_address: Option<&str>,
    consume_nonce: bool,
) -> Result<serde_json::Value, Response> {
    let payload = if envelope.payload.is_object() {
        envelope.payload.clone()
    } else {
        return Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Signed intent payload must be an object",
        ));
    };

    let payload_address = payload
        .get("address")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_intent",
                "Intent address is required",
            )
        })?;
    let intent_type = payload
        .get("intentType")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_intent",
                "Intent type is required",
            )
        })?;
    let expires_at = payload
        .get("expiresAt")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_intent",
                "Intent expiry is required",
            )
        })?;
    let network = payload
        .get("network")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_intent",
                "Intent network is required",
            )
        })?;
    let chain_id = payload
        .get("chainId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_intent",
                "Intent chainId is required",
            )
        })?;

    if intent_type != expected_intent_type {
        return Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            format!(
                "Intent type mismatch: expected '{}', got '{}'",
                expected_intent_type, intent_type
            ),
        ));
    }
    if envelope.signing_address != payload_address {
        return Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "signingAddress must match payload address",
        ));
    }
    if let Some(expected_address) = expected_address {
        if expected_address != payload_address {
            return Err(marketplace_error(
                StatusCode::FORBIDDEN,
                "invalid_intent",
                "Intent address does not match the expected actor",
            ));
        }
    }

    let expected_network = marketplace_network_name(state.dogecoin_config.network);
    if network != expected_network {
        return Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            format!(
                "Intent network mismatch: expected '{}', got '{}'",
                expected_network, network
            ),
        ));
    }

    let expected_chain_id = marketplace_chain_id(state.dogecoin_config.network);
    if chain_id != expected_chain_id {
        return Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            format!(
                "Intent chainId mismatch: expected '{}', got '{}'",
                expected_chain_id, chain_id
            ),
        ));
    }

    let expires_at = marketplace_parse_timestamp(expires_at, "expiresAt")?;
    if expires_at <= chrono::Utc::now() {
        return Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent has expired",
        ));
    }

    let canonical_payload = marketplace_canonicalize_json(&payload);
    let canonical_json = serde_json::to_string(&canonical_payload).map_err(|_| {
        marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_intent",
            "Failed to canonicalize intent payload",
        )
    })?;
    let computed_payload_hash = sha256d::Hash::hash(canonical_json.as_bytes()).to_string();
    if computed_payload_hash != envelope.payload_hash {
        return Err(marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Signed intent payloadHash does not match the canonical payload",
        ));
    }

    marketplace_verify_message_signature(
        state,
        &envelope.signing_address,
        &canonical_json,
        &envelope.signature,
    )
    .await?;

    if consume_nonce {
        marketplace_consume_intent_nonce(client, &payload, &computed_payload_hash).await?;
    }

    Ok(payload)
}

fn marketplace_decode_raw_transaction(raw_tx_hex: &str) -> Result<Transaction, Response> {
    let raw_tx_bytes = hex::decode(raw_tx_hex).map_err(|_| {
        marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_transaction",
            "signedPsbt must contain raw transaction hex",
        )
    })?;

    deserialize::<Transaction>(&raw_tx_bytes).map_err(|_| {
        marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_transaction",
            "Unable to decode raw transaction",
        )
    })
}

fn marketplace_output_value_for_script(transaction: &Transaction, script: &ScriptBuf) -> i64 {
    transaction
        .output
        .iter()
        .filter(|output| output.script_pubkey == *script)
        .map(|output| output.value.to_sat() as i64)
        .sum()
}

async fn marketplace_broadcast_raw_transaction(
    state: &AppState,
    raw_tx_hex: &str,
) -> Result<String, Response> {
    let ctx = dogecoin::utils::Context::empty();
    let rpc = dogecoin::utils::dogecoind::dogecoin_get_client(&state.dogecoin_config, &ctx);
    rpc.call::<String>(
        "sendrawtransaction",
        &[serde_json::to_value(raw_tx_hex).unwrap_or(serde_json::Value::Null)],
    )
    .map_err(|_| {
        marketplace_error(
            StatusCode::BAD_GATEWAY,
            "broadcast_failed",
            "Failed to broadcast raw Dogecoin transaction",
        )
    })
}

async fn marketplace_fetch_tx_status(
    state: &AppState,
    txid: &str,
) -> Result<serde_json::Value, Response> {
    let ctx = dogecoin::utils::Context::empty();
    let rpc = dogecoin::utils::dogecoind::dogecoin_get_client(&state.dogecoin_config, &ctx);
    let response = rpc
        .call::<serde_json::Value>(
            "getrawtransaction",
            &[
                serde_json::to_value(txid).unwrap_or(serde_json::Value::Null),
                serde_json::Value::Bool(true),
            ],
        )
        .map_err(|_| {
            marketplace_error(
                StatusCode::NOT_FOUND,
                "tx_not_found",
                format!("Transaction '{}' was not found", txid),
            )
        })?;

    let confirmations = response
        .get("confirmations")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    Ok(json!({
        "txid": txid,
        "confirmations": confirmations,
        "finalized": confirmations >= 6,
        "status": if confirmations >= 6 {
            "finalized"
        } else if confirmations > 0 {
            "confirming"
        } else {
            "pending"
        },
    }))
}

pub async fn marketplace_health(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "kabosu-dmp",
        "network": format!("{:?}", state.dogecoin_config.network),
    }))
}

pub async fn marketplace_sync(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let latest_indexed_block = client
        .query_opt(
            "SELECT block_height::bigint
             FROM inscriptions
             ORDER BY number DESC
             LIMIT 1",
            &[],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|row| row.get::<_, i64>(0));

    Ok(Json(json!({
        "status": "ok",
        "network": format!("{:?}", state.dogecoin_config.network),
        "latestIndexedBlock": latest_indexed_block,
        "syncLagBlocks": serde_json::Value::Null,
        "reorgDepth": serde_json::Value::Null,
    })))
}

fn marketplace_listing_row(row: &Row) -> serde_json::Value {
    let settlement_txid: Option<String> = row.get("settlement_txid");
    let settlement = settlement_txid.as_ref().map(|txid| {
        json!({
            "txid": txid,
            "blockHeight": row.get::<_, Option<i64>>("settlement_block_height"),
            "confirmations": row.get::<_, i32>("settlement_confirmations"),
            "finalizedAt": row.get::<_, Option<String>>("finalized_at"),
        })
    });

    json!({
        "id": row.get::<_, String>("id"),
        "inscriptionId": row.get::<_, String>("inscription_id"),
        "collectionId": row.get::<_, Option<String>>("collection_id"),
        "sellerAddress": row.get::<_, String>("seller_address"),
        "askingPriceKoinu": row.get::<_, i64>("asking_price_koinu").to_string(),
        "currency": row.get::<_, String>("currency"),
        "marketplaceFeeBps": row.get::<_, i32>("marketplace_fee_bps"),
        "royaltyBps": row.get::<_, Option<i32>>("royalty_bps"),
        "status": row.get::<_, String>("status"),
        "expiryAt": row.get::<_, Option<String>>("expiry_at"),
        "createdAt": row.get::<_, String>("created_at"),
        "updatedAt": row.get::<_, String>("updated_at"),
        "settlement": settlement,
    })
}

fn marketplace_offer_row(row: &Row) -> serde_json::Value {
    json!({
        "id": row.get::<_, String>("id"),
        "scope": row.get::<_, String>("scope"),
        "inscriptionId": row.get::<_, Option<String>>("inscription_id"),
        "collectionId": row.get::<_, Option<String>>("collection_id"),
        "makerAddress": row.get::<_, String>("maker_address"),
        "targetSellerAddress": row.get::<_, Option<String>>("target_seller_address"),
        "offerPriceKoinu": row.get::<_, i64>("offer_price_koinu").to_string(),
        "marketplaceFeeBps": row.get::<_, i32>("marketplace_fee_bps"),
        "status": row.get::<_, String>("status"),
        "expiresAt": row.get::<_, String>("expires_at"),
        "createdAt": row.get::<_, String>("created_at"),
        "updatedAt": row.get::<_, String>("updated_at"),
    })
}

fn marketplace_auction_row(row: &Row) -> serde_json::Value {
    let highest_bid_id: Option<String> = row.get("highest_bid_id");
    let highest_bid = highest_bid_id.as_ref().map(|bid_id| {
        json!({
            "bidId": bid_id,
            "bidderAddress": row.get::<_, Option<String>>("highest_bidder_address"),
            "amountKoinu": row
                .get::<_, Option<i64>>("highest_bid_amount_koinu")
                .map(|value| value.to_string()),
            "placedAt": row.get::<_, Option<String>>("highest_bid_placed_at"),
        })
    });

    json!({
        "id": row.get::<_, String>("id"),
        "inscriptionId": row.get::<_, String>("inscription_id"),
        "sellerAddress": row.get::<_, String>("seller_address"),
        "startPriceKoinu": row.get::<_, i64>("start_price_koinu").to_string(),
        "reservePriceKoinu": row.get::<_, Option<i64>>("reserve_price_koinu").map(|value| value.to_string()),
        "minIncrementKoinu": row.get::<_, i64>("min_increment_koinu").to_string(),
        "startsAt": row.get::<_, String>("starts_at"),
        "endsAt": row.get::<_, String>("ends_at"),
        "status": row.get::<_, String>("status"),
        "highestBid": highest_bid,
    })
}

fn marketplace_bid_row(row: &Row) -> serde_json::Value {
    json!({
        "id": row.get::<_, String>("id"),
        "auctionId": row.get::<_, String>("auction_id"),
        "bidderAddress": row.get::<_, String>("bidder_address"),
        "amountKoinu": row.get::<_, i64>("bid_amount_koinu").to_string(),
        "status": row.get::<_, String>("status"),
        "bidderSignature": {
            "payloadHash": row.get::<_, Option<String>>("bidder_signature_payload_hash"),
            "signature": row.get::<_, Option<String>>("bidder_signature"),
            "signingAddress": row.get::<_, Option<String>>("bidder_signing_address"),
            "signedAt": row.get::<_, Option<String>>("bidder_signed_at"),
        },
        "createdAt": row.get::<_, String>("created_at"),
        "updatedAt": row.get::<_, String>("updated_at"),
    })
}

fn marketplace_trader_row(row: &Row) -> serde_json::Value {
    let x_verified = row.get::<_, bool>("x_verified");
    let badges = if x_verified {
        vec!["x_verified".to_string()]
    } else {
        Vec::new()
    };

    json!({
        "id": row.get::<_, String>("address"),
        "address": row.get::<_, String>("address"),
        "displayName": row.get::<_, Option<String>>("display_name"),
        "bio": row.get::<_, Option<String>>("bio"),
        "avatarUrl": row.get::<_, Option<String>>("avatar_url"),
        "xHandle": row.get::<_, Option<String>>("x_handle"),
        "xUserId": row.get::<_, Option<String>>("x_user_id"),
        "xVerified": x_verified,
        "xVerifiedAt": row.get::<_, Option<String>>("x_verified_at"),
        "verificationLevel": if x_verified { "x_verified" } else { "wallet_verified" },
        "badges": badges,
        "metrics": {
            "totalSalesKoinu": "0",
            "totalBuysKoinu": "0",
            "successfulTrades": 0,
            "offersAccepted": 0,
            "auctionsWon": 0,
            "fulfillmentRateBps": 0,
        },
        "createdAt": row.get::<_, String>("created_at"),
        "updatedAt": row.get::<_, String>("updated_at"),
    })
}

fn marketplace_default_trader(address: &str) -> serde_json::Value {
    let now = marketplace_now();
    json!({
        "id": address,
        "address": address,
        "displayName": serde_json::Value::Null,
        "bio": serde_json::Value::Null,
        "avatarUrl": serde_json::Value::Null,
        "xHandle": serde_json::Value::Null,
        "xUserId": serde_json::Value::Null,
        "xVerified": false,
        "xVerifiedAt": serde_json::Value::Null,
        "verificationLevel": "none",
        "badges": [],
        "metrics": {
            "totalSalesKoinu": "0",
            "totalBuysKoinu": "0",
            "successfulTrades": 0,
            "offersAccepted": 0,
            "auctionsWon": 0,
            "fulfillmentRateBps": 0,
        },
        "createdAt": now,
        "updatedAt": now,
    })
}

fn marketplace_activity_row(row: &Row) -> serde_json::Value {
    json!({
        "id": row.get::<_, i64>("id").to_string(),
        "type": row.get::<_, String>("event_type"),
        "actorAddress": row.get::<_, String>("trader_address"),
        "subjectId": row.get::<_, String>("entity_id"),
        "inscriptionId": row.get::<_, Option<String>>("inscription_id"),
        "amountKoinu": row.get::<_, Option<i64>>("amount_koinu").map(|value| value.to_string()),
        "txid": row.get::<_, Option<String>>("txid"),
        "metadata": marketplace_parse_metadata(row.get::<_, Option<String>>("metadata")),
        "createdAt": row.get::<_, String>("created_at"),
    })
}

async fn marketplace_upsert_trader(
    client: &deadpool_postgres::Client,
    address: &str,
) -> Result<(), StatusCode> {
    let now = marketplace_now();
    client
        .execute(
            "INSERT INTO marketplace_traders (address, created_at, updated_at)
             VALUES ($1, $2, $2)
             ON CONFLICT (address)
             DO UPDATE SET updated_at = marketplace_traders.updated_at",
            &[&address, &now],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(())
}

async fn marketplace_insert_activity(
    client: &deadpool_postgres::Client,
    trader_address: &str,
    event_type: &str,
    entity_type: &str,
    entity_id: &str,
    inscription_id: Option<&String>,
    amount_koinu: Option<i64>,
    txid: Option<&String>,
    metadata: Option<&serde_json::Value>,
) -> Result<(), StatusCode> {
    let now = marketplace_now();
    let metadata_text = marketplace_metadata_text(metadata);
    client
        .execute(
            "INSERT INTO marketplace_activity (
                 trader_address, event_type, entity_type, entity_id,
                 inscription_id, amount_koinu, txid, metadata, created_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            &[
                &trader_address,
                &event_type,
                &entity_type,
                &entity_id,
                &inscription_id,
                &amount_koinu,
                &txid,
                &metadata_text,
                &now,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(())
}

pub async fn list_marketplace_listings(
    State(state): State<AppState>,
    Query(params): Query<MarketplaceFeedParams>,
) -> Result<Json<MarketplaceListResponse<serde_json::Value>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let offset = marketplace_offset(&params);
    let min_price = marketplace_parse_optional_koinu(&params.min_price, "minPrice")
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let max_price = marketplace_parse_optional_koinu(&params.max_price, "maxPrice")
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let sort_sql = match params.sort.as_deref() {
        Some("price_asc") => "asking_price_koinu ASC, created_at DESC",
        Some("price_desc") => "asking_price_koinu DESC, created_at DESC",
        _ => "created_at DESC",
    };

    let count_row = client
        .query_one(
            "SELECT COUNT(*)::bigint
             FROM marketplace_listings
             WHERE ($1::text IS NULL OR status = $1)
               AND ($2::text IS NULL OR collection_id = $2)
               AND ($3::text IS NULL OR seller_address = $3)
               AND ($4::text IS NULL OR inscription_id = $4)
               AND ($5::bigint IS NULL OR asking_price_koinu >= $5)
               AND ($6::bigint IS NULL OR asking_price_koinu <= $6)",
            &[
                &params.status,
                &params.collection_id,
                &params.seller_address,
                &params.inscription_id,
                &min_price,
                &max_price,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total: i64 = count_row.get(0);

    let query = format!(
        "SELECT id, inscription_id, collection_id, seller_address, asking_price_koinu,
                currency, marketplace_fee_bps, royalty_bps, status, expiry_at,
                settlement_txid, settlement_block_height, settlement_confirmations,
                finalized_at, created_at, updated_at
         FROM marketplace_listings
         WHERE ($1::text IS NULL OR status = $1)
           AND ($2::text IS NULL OR collection_id = $2)
           AND ($3::text IS NULL OR seller_address = $3)
           AND ($4::text IS NULL OR inscription_id = $4)
           AND ($5::bigint IS NULL OR asking_price_koinu >= $5)
           AND ($6::bigint IS NULL OR asking_price_koinu <= $6)
         ORDER BY {}
         LIMIT $7 OFFSET $8",
        sort_sql
    );

    let rows = client
        .query(
            &query,
            &[
                &params.status,
                &params.collection_id,
                &params.seller_address,
                &params.inscription_id,
                &min_price,
                &max_price,
                &params.limit,
                &offset,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(MarketplaceListResponse {
        items: rows.iter().map(marketplace_listing_row).collect(),
        total: total.max(0) as usize,
        next_cursor: marketplace_next_cursor(offset, params.limit, total),
    }))
}

pub async fn get_marketplace_listing(
    State(state): State<AppState>,
    Path(listing_id): Path<String>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    match client
        .query_opt(
            "SELECT id, inscription_id, collection_id, seller_address, asking_price_koinu,
                    currency, marketplace_fee_bps, royalty_bps, status, expiry_at,
                    settlement_txid, settlement_block_height, settlement_confirmations,
                    finalized_at, created_at, updated_at
             FROM marketplace_listings
             WHERE id = $1",
            &[&listing_id],
        )
        .await
    {
        Ok(Some(row)) => Json(marketplace_listing_row(&row)).into_response(),
        Ok(None) => marketplace_error(
            StatusCode::NOT_FOUND,
            "listing_not_found",
            format!("Marketplace listing '{}' was not found", listing_id),
        ),
        Err(_) => marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to load listing",
        ),
    }
}

pub async fn create_marketplace_listing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateMarketplaceListingRequest>,
) -> impl IntoResponse {
    let idempotency_key = match marketplace_idempotency_key(&headers) {
        Ok(key) => key,
        Err(response) => return response,
    };

    let asking_price_koinu =
        match marketplace_parse_koinu(&body.asking_price_koinu, "askingPriceKoinu") {
            Ok(value) => value,
            Err(response) => return response,
        };

    if body.seller_address.trim().is_empty() || body.inscription_id.trim().is_empty() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_listing",
            "sellerAddress and inscriptionId are required",
        );
    }

    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    if let Err(response) =
        marketplace_require_session(&client, &headers, &body.seller_address).await
    {
        return response;
    }

    match client
        .query_opt(
            "SELECT id, inscription_id, collection_id, seller_address, asking_price_koinu,
                    currency, marketplace_fee_bps, royalty_bps, status, expiry_at,
                    settlement_txid, settlement_block_height, settlement_confirmations,
                    finalized_at, created_at, updated_at
             FROM marketplace_listings
             WHERE idempotency_key = $1",
            &[&idempotency_key],
        )
        .await
    {
        Ok(Some(row)) => return Json(marketplace_listing_row(&row)).into_response(),
        Ok(None) => {}
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to check listing idempotency",
            )
        }
    }

    if marketplace_upsert_trader(&client, &body.seller_address)
        .await
        .is_err()
    {
        return marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to prepare trader profile",
        );
    }

    let listing_id = marketplace_id("lst");
    let now = marketplace_now();
    let seller_signed_template = marketplace_metadata_text(body.seller_signed_template.as_ref());

    let row = match client
        .query_one(
            "INSERT INTO marketplace_listings (
                 id, inscription_id, collection_id, seller_address, asking_price_koinu,
                 currency, marketplace_fee_bps, royalty_bps, status, expiry_at,
                 seller_signed_template, created_at, updated_at, idempotency_key
             ) VALUES (
                 $1, $2, $3, $4, $5,
                 'DOGE', $6, $7, 'active', $8,
                 $9, $10, $10, $11
             )
             RETURNING id, inscription_id, collection_id, seller_address, asking_price_koinu,
                       currency, marketplace_fee_bps, royalty_bps, status, expiry_at,
                       settlement_txid, settlement_block_height, settlement_confirmations,
                       finalized_at, created_at, updated_at",
            &[
                &listing_id,
                &body.inscription_id,
                &body.collection_id,
                &body.seller_address,
                &asking_price_koinu,
                &body.marketplace_fee_bps.unwrap_or(0),
                &body.royalty_bps,
                &body.expiry_at,
                &seller_signed_template,
                &now,
                &idempotency_key,
            ],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to create marketplace listing",
            )
        }
    };

    let activity_metadata = json!({
        "collectionId": body.collection_id,
        "askingPriceKoinu": asking_price_koinu.to_string(),
    });
    let _ = marketplace_insert_activity(
        &client,
        &body.seller_address,
        "listing_created",
        "listing",
        &listing_id,
        Some(&body.inscription_id),
        Some(asking_price_koinu),
        None,
        Some(&activity_metadata),
    )
    .await;

    (StatusCode::CREATED, Json(marketplace_listing_row(&row))).into_response()
}

pub async fn cancel_marketplace_listing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<String>,
    Json(body): Json<CancelMarketplaceListingRequest>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    if let Err(response) =
        marketplace_require_session(&client, &headers, &body.seller_address).await
    {
        return response;
    }

    let existing = match client
        .query_opt(
            "SELECT seller_address, inscription_id, status
             FROM marketplace_listings
             WHERE id = $1",
            &[&listing_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return marketplace_error(
                StatusCode::NOT_FOUND,
                "listing_not_found",
                format!("Marketplace listing '{}' was not found", listing_id),
            )
        }
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to load listing for cancellation",
            )
        }
    };

    let seller_address: String = existing.get("seller_address");
    let inscription_id: String = existing.get("inscription_id");
    let status: String = existing.get("status");

    if seller_address != body.seller_address {
        return marketplace_error(
            StatusCode::FORBIDDEN,
            "listing_cancel_forbidden",
            "sellerAddress does not match the listing owner",
        );
    }
    if status == "cancelled" {
        return marketplace_error(
            StatusCode::CONFLICT,
            "listing_already_cancelled",
            "Listing is already cancelled",
        );
    }

    let now = marketplace_now();
    let row = match client
        .query_one(
            "UPDATE marketplace_listings
             SET status = 'cancelled', updated_at = $2
             WHERE id = $1
             RETURNING id, inscription_id, collection_id, seller_address, asking_price_koinu,
                       currency, marketplace_fee_bps, royalty_bps, status, expiry_at,
                       settlement_txid, settlement_block_height, settlement_confirmations,
                       finalized_at, created_at, updated_at",
            &[&listing_id, &now],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to cancel listing",
            )
        }
    };

    let _ = marketplace_insert_activity(
        &client,
        &seller_address,
        "listing_cancelled",
        "listing",
        &listing_id,
        Some(&inscription_id),
        None,
        None,
        None,
    )
    .await;

    Json(marketplace_listing_row(&row)).into_response()
}

pub async fn create_marketplace_auth_challenge(
    State(state): State<AppState>,
    Json(body): Json<MarketplaceAuthChallengeRequest>,
) -> impl IntoResponse {
    if body.address.trim().is_empty() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_auth_request",
            "address is required",
        );
    }

    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    let challenge_id = marketplace_id("auth");
    let challenge_token = marketplace_random_token("challenge");
    let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    let created_at = marketplace_now();
    let message = format!(
        "wzrd.dog marketplace auth\naddress:{}\nchallenge:{}\nexpiresAt:{}",
        body.address, challenge_token, expires_at
    );

    match client
        .execute(
            "INSERT INTO marketplace_auth_challenges (
                 id, address, challenge_token, message, expires_at, created_at
             ) VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &challenge_id,
                &body.address,
                &challenge_token,
                &message,
                &expires_at,
                &created_at,
            ],
        )
        .await
    {
        Ok(_) => {}
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to create marketplace auth challenge",
            )
        }
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "challengeId": challenge_id,
            "address": body.address,
            "message": message,
            "expiresAt": expires_at,
        })),
    )
        .into_response()
}

pub async fn verify_marketplace_auth_challenge(
    State(state): State<AppState>,
    Json(body): Json<MarketplaceAuthVerifyRequest>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    let challenge = match client
        .query_opt(
            "SELECT message, expires_at, used_at
             FROM marketplace_auth_challenges
             WHERE id = $1 AND address = $2",
            &[&body.challenge_id, &body.address],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return marketplace_error(
                StatusCode::NOT_FOUND,
                "challenge_not_found",
                "Marketplace auth challenge was not found",
            )
        }
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to load marketplace auth challenge",
            )
        }
    };

    let used_at: Option<String> = challenge.get("used_at");
    if used_at.is_some() {
        return marketplace_error(
            StatusCode::CONFLICT,
            "challenge_used",
            "Marketplace auth challenge has already been used",
        );
    }

    let expires_at: String = challenge.get("expires_at");
    let expires_at = match marketplace_parse_timestamp(&expires_at, "expiresAt") {
        Ok(expires_at) => expires_at,
        Err(response) => return response,
    };
    if expires_at <= chrono::Utc::now() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "challenge_expired",
            "Marketplace auth challenge has expired",
        );
    }

    let message: String = challenge.get("message");
    if let Err(response) =
        marketplace_verify_message_signature(&state, &body.address, &message, &body.signature).await
    {
        return response;
    }

    let now = marketplace_now();
    match client
        .execute(
            "UPDATE marketplace_auth_challenges
             SET used_at = $2
             WHERE id = $1 AND used_at IS NULL",
            &[&body.challenge_id, &now],
        )
        .await
    {
        Ok(1) => {}
        Ok(_) => {
            return marketplace_error(
                StatusCode::CONFLICT,
                "challenge_used",
                "Marketplace auth challenge has already been consumed",
            )
        }
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to finalize marketplace auth challenge",
            )
        }
    }

    if marketplace_upsert_trader(&client, &body.address)
        .await
        .is_err()
    {
        return marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to prepare trader profile",
        );
    }

    let session_token = marketplace_random_token("session");
    let session_expires_at = (chrono::Utc::now() + chrono::Duration::hours(12)).to_rfc3339();
    match client
        .execute(
            "INSERT INTO marketplace_sessions (
                 token, address, expires_at, revoked_at, created_at, last_used_at
             ) VALUES ($1, $2, $3, NULL, $4, $4)",
            &[&session_token, &body.address, &session_expires_at, &now],
        )
        .await
    {
        Ok(_) => {}
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to create marketplace session",
            )
        }
    }

    Json(json!({
        "address": body.address,
        "sessionToken": session_token,
        "expiresAt": session_expires_at,
    }))
    .into_response()
}

pub async fn build_marketplace_order(
    State(state): State<AppState>,
    Path(listing_id): Path<String>,
    Json(body): Json<BuildMarketplaceOrderRequest>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    let payload = match marketplace_verify_signed_intent(
        &state,
        &client,
        &body.signed_intent,
        "listing_buy",
        None,
        false,
    )
    .await
    {
        Ok(payload) => payload,
        Err(response) => return response,
    };

    let listing = match client
        .query_opt(
            "SELECT id, inscription_id, collection_id, seller_address, asking_price_koinu,
                    currency, marketplace_fee_bps, royalty_bps, status, expiry_at,
                    settlement_txid, settlement_block_height, settlement_confirmations,
                    finalized_at, created_at, updated_at
             FROM marketplace_listings
             WHERE id = $1",
            &[&listing_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return marketplace_error(
                StatusCode::NOT_FOUND,
                "listing_not_found",
                format!("Marketplace listing '{}' was not found", listing_id),
            )
        }
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to load marketplace listing",
            )
        }
    };

    let listing_json = marketplace_listing_row(&listing);
    let status = listing_json
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    if status != "active" {
        return marketplace_error(
            StatusCode::CONFLICT,
            "listing_not_buyable",
            format!("Listing cannot be purchased while in '{}' state", status),
        );
    }

    if payload
        .get("listingId")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value != listing_id)
    {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent listingId does not match the requested listing",
        );
    }

    let buyer_address = payload
        .get("address")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let seller_address = listing.get::<_, String>("seller_address");
    let asking_price_koinu = listing.get::<_, i64>("asking_price_koinu");

    Json(json!({
        "listing": listing_json,
        "buyerAddress": buyer_address,
        "sellerAddress": seller_address,
        "requiredOutputs": [
            {
                "address": seller_address,
                "amountKoinu": asking_price_koinu.to_string(),
            }
        ],
        "feePolicy": {
            "currency": "DOGE",
            "marketplaceFeeBps": listing.get::<_, i32>("marketplace_fee_bps"),
            "buyerPaysKoinu": asking_price_koinu.to_string(),
            "sellerReceivesKoinu": asking_price_koinu.to_string(),
        },
        "submitFormat": "raw_tx_hex_in_signedPsbt_field",
        "intent": {
            "payloadHash": body.signed_intent.payload_hash,
            "signedAt": body.signed_intent.signed_at,
        }
    }))
    .into_response()
}

pub async fn submit_marketplace_order(
    State(state): State<AppState>,
    Path(listing_id): Path<String>,
    Json(body): Json<SubmitMarketplaceOrderRequest>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    let payload = match marketplace_verify_signed_intent(
        &state,
        &client,
        &body.signed_intent,
        "listing_buy",
        None,
        true,
    )
    .await
    {
        Ok(payload) => payload,
        Err(response) => return response,
    };

    let listing = match client
        .query_opt(
            "SELECT id, inscription_id, collection_id, seller_address, asking_price_koinu,
                    currency, marketplace_fee_bps, royalty_bps, status, expiry_at,
                    settlement_txid, settlement_block_height, settlement_confirmations,
                    finalized_at, created_at, updated_at
             FROM marketplace_listings
             WHERE id = $1",
            &[&listing_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return marketplace_error(
                StatusCode::NOT_FOUND,
                "listing_not_found",
                format!("Marketplace listing '{}' was not found", listing_id),
            )
        }
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to load marketplace listing",
            )
        }
    };

    let current_status: String = listing.get("status");
    if current_status != "active" {
        return marketplace_error(
            StatusCode::CONFLICT,
            "listing_not_buyable",
            format!(
                "Listing cannot be submitted while in '{}' state",
                current_status
            ),
        );
    }

    if payload
        .get("listingId")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value != listing_id)
    {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent listingId does not match the requested listing",
        );
    }

    let seller_address: String = listing.get("seller_address");
    let inscription_id: String = listing.get("inscription_id");
    let asking_price_koinu: i64 = listing.get("asking_price_koinu");
    let buyer_address = payload
        .get("address")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();

    let transaction = match marketplace_decode_raw_transaction(&body.signed_psbt) {
        Ok(transaction) => transaction,
        Err(response) => return response,
    };
    let seller_script = match marketplace_parse_dogecoin_address(&seller_address) {
        Ok(script) => script,
        Err(response) => return response,
    };
    let seller_paid_koinu = marketplace_output_value_for_script(&transaction, &seller_script);
    if seller_paid_koinu < asking_price_koinu {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "insufficient_payment",
            format!(
                "Transaction pays {} koinu to seller but listing requires {}",
                seller_paid_koinu, asking_price_koinu
            ),
        );
    }

    let txid = match marketplace_broadcast_raw_transaction(&state, &body.signed_psbt).await {
        Ok(txid) => txid,
        Err(response) => return response,
    };
    let now = marketplace_now();
    let txid_string = txid.clone();
    let row = match client
        .query_one(
            "UPDATE marketplace_listings
             SET status = 'sold_pending_settlement',
                 settlement_txid = $2,
                 settlement_confirmations = 0,
                 updated_at = $3
             WHERE id = $1
             RETURNING id, inscription_id, collection_id, seller_address, asking_price_koinu,
                       currency, marketplace_fee_bps, royalty_bps, status, expiry_at,
                       settlement_txid, settlement_block_height, settlement_confirmations,
                       finalized_at, created_at, updated_at",
            &[&listing_id, &txid_string, &now],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to update marketplace listing after broadcast",
            )
        }
    };

    let seller_metadata = json!({
        "buyerAddress": buyer_address,
        "listingId": listing_id,
    });
    let _ = marketplace_insert_activity(
        &client,
        &seller_address,
        "listing_sold",
        "listing",
        &listing_id,
        Some(&inscription_id),
        Some(asking_price_koinu),
        Some(&txid_string),
        Some(&seller_metadata),
    )
    .await;

    if buyer_address != seller_address {
        let buyer_metadata = json!({
            "sellerAddress": seller_address,
            "listingId": listing_id,
        });
        let _ = marketplace_insert_activity(
            &client,
            &buyer_address,
            "listing_sold",
            "listing",
            &listing_id,
            Some(&inscription_id),
            Some(asking_price_koinu),
            Some(&txid_string),
            Some(&buyer_metadata),
        )
        .await;
    }

    Json(json!({
        "listing": marketplace_listing_row(&row),
        "txid": txid_string,
        "confirmations": 0,
        "finalized": false,
        "status": "sold_pending_settlement",
    }))
    .into_response()
}

pub async fn get_marketplace_tx_status(
    State(state): State<AppState>,
    Path(txid): Path<String>,
) -> impl IntoResponse {
    match marketplace_fetch_tx_status(&state, &txid).await {
        Ok(status) => Json(status).into_response(),
        Err(_) => Json(json!({
            "txid": txid,
            "confirmations": 0,
            "finalized": false,
            "status": "pending",
        }))
        .into_response(),
    }
}

pub async fn get_marketplace_trader(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    match client
        .query_opt(
            "SELECT address, display_name, bio, avatar_url, x_handle, x_user_id,
                    x_verified, x_verified_at, created_at, updated_at
             FROM marketplace_traders
             WHERE address = $1",
            &[&address],
        )
        .await
    {
        Ok(Some(row)) => Json(marketplace_trader_row(&row)).into_response(),
        Ok(None) => Json(marketplace_default_trader(&address)).into_response(),
        Err(_) => marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to load trader profile",
        ),
    }
}

pub async fn update_marketplace_trader(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(address): Path<String>,
    Json(body): Json<UpdateMarketplaceTraderRequest>,
) -> impl IntoResponse {
    if address.trim().is_empty() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_trader",
            "address is required",
        );
    }

    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    if let Err(response) = marketplace_require_session(&client, &headers, &address).await {
        return response;
    }

    if marketplace_upsert_trader(&client, &address).await.is_err() {
        return marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to prepare trader profile",
        );
    }

    let now = marketplace_now();
    let row = match client
        .query_one(
            "UPDATE marketplace_traders
             SET display_name = COALESCE($2, display_name),
                 bio = COALESCE($3, bio),
                 avatar_url = COALESCE($4, avatar_url),
                 updated_at = $5
             WHERE address = $1
             RETURNING address, display_name, bio, avatar_url, x_handle, x_user_id,
                       x_verified, x_verified_at, created_at, updated_at",
            &[
                &address,
                &body.display_name,
                &body.bio,
                &body.avatar_url,
                &now,
            ],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to update trader profile",
            )
        }
    };

    let activity_metadata = json!({
        "displayName": body.display_name,
        "bio": body.bio,
        "avatarUrl": body.avatar_url,
    });
    let _ = marketplace_insert_activity(
        &client,
        &address,
        "profile_updated",
        "trader",
        &address,
        None,
        None,
        None,
        Some(&activity_metadata),
    )
    .await;

    Json(marketplace_trader_row(&row)).into_response()
}

pub async fn verify_marketplace_trader_x(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(address): Path<String>,
    Json(body): Json<VerifyMarketplaceXRequest>,
) -> impl IntoResponse {
    if body.x_handle.trim().is_empty() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_x_profile",
            "xHandle is required",
        );
    }

    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    if let Err(response) = marketplace_require_session(&client, &headers, &address).await {
        return response;
    }

    if marketplace_upsert_trader(&client, &address).await.is_err() {
        return marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to prepare trader profile",
        );
    }

    let now = marketplace_now();
    let x_handle = body.x_handle.trim().trim_start_matches('@').to_string();
    let row = match client
        .query_one(
            "UPDATE marketplace_traders
             SET x_handle = $2,
                 x_user_id = $3,
                 x_verified = TRUE,
                 x_verified_at = $4,
                 updated_at = $4
             WHERE address = $1
             RETURNING address, display_name, bio, avatar_url, x_handle, x_user_id,
                       x_verified, x_verified_at, created_at, updated_at",
            &[&address, &x_handle, &body.x_user_id, &now],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to verify X profile",
            )
        }
    };

    let activity_metadata = json!({
        "xHandle": x_handle,
        "xUserId": body.x_user_id,
    });
    let _ = marketplace_insert_activity(
        &client,
        &address,
        "x_verified",
        "trader",
        &address,
        None,
        None,
        None,
        Some(&activity_metadata),
    )
    .await;

    Json(marketplace_trader_row(&row)).into_response()
}

pub async fn get_marketplace_trader_activity(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(params): Query<MarketplaceFeedParams>,
) -> Result<Json<MarketplaceListResponse<serde_json::Value>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let offset = marketplace_offset(&params);
    let count_row = client
        .query_one(
            "SELECT COUNT(*)::bigint
             FROM marketplace_activity
             WHERE trader_address = $1",
            &[&address],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total: i64 = count_row.get(0);

    let rows = client
        .query(
            "SELECT id, trader_address, event_type, entity_id, inscription_id,
                    amount_koinu, txid, metadata, created_at
             FROM marketplace_activity
             WHERE trader_address = $1
             ORDER BY id DESC
             LIMIT $2 OFFSET $3",
            &[&address, &params.limit, &offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(MarketplaceListResponse {
        items: rows.iter().map(marketplace_activity_row).collect(),
        total: total.max(0) as usize,
        next_cursor: marketplace_next_cursor(offset, params.limit, total),
    }))
}

pub async fn list_marketplace_offers(
    State(state): State<AppState>,
    Query(params): Query<MarketplaceFeedParams>,
) -> Result<Json<MarketplaceListResponse<serde_json::Value>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let offset = marketplace_offset(&params);
    let min_price = marketplace_parse_optional_koinu(&params.min_price, "minPrice")
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let max_price = marketplace_parse_optional_koinu(&params.max_price, "maxPrice")
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let sort_sql = match params.sort.as_deref() {
        Some("price_asc") => "offer_price_koinu ASC, created_at DESC",
        Some("price_desc") => "offer_price_koinu DESC, created_at DESC",
        _ => "created_at DESC",
    };

    let count_row = client
        .query_one(
            "SELECT COUNT(*)::bigint
             FROM marketplace_offers
             WHERE ($1::text IS NULL OR status = $1)
               AND ($2::text IS NULL OR collection_id = $2)
               AND ($3::text IS NULL OR maker_address = $3)
               AND ($4::text IS NULL OR inscription_id = $4)
               AND ($5::bigint IS NULL OR offer_price_koinu >= $5)
               AND ($6::bigint IS NULL OR offer_price_koinu <= $6)",
            &[
                &params.status,
                &params.collection_id,
                &params.maker_address,
                &params.inscription_id,
                &min_price,
                &max_price,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total: i64 = count_row.get(0);

    let query = format!(
        "SELECT id, scope, inscription_id, collection_id, maker_address,
                target_seller_address, offer_price_koinu, marketplace_fee_bps,
                status, expires_at, created_at, updated_at
         FROM marketplace_offers
         WHERE ($1::text IS NULL OR status = $1)
           AND ($2::text IS NULL OR collection_id = $2)
           AND ($3::text IS NULL OR maker_address = $3)
           AND ($4::text IS NULL OR inscription_id = $4)
           AND ($5::bigint IS NULL OR offer_price_koinu >= $5)
           AND ($6::bigint IS NULL OR offer_price_koinu <= $6)
         ORDER BY {}
         LIMIT $7 OFFSET $8",
        sort_sql
    );

    let rows = client
        .query(
            &query,
            &[
                &params.status,
                &params.collection_id,
                &params.maker_address,
                &params.inscription_id,
                &min_price,
                &max_price,
                &params.limit,
                &offset,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(MarketplaceListResponse {
        items: rows.iter().map(marketplace_offer_row).collect(),
        total: total.max(0) as usize,
        next_cursor: marketplace_next_cursor(offset, params.limit, total),
    }))
}

pub async fn create_marketplace_offer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateMarketplaceOfferRequest>,
) -> impl IntoResponse {
    let idempotency_key = match marketplace_idempotency_key(&headers) {
        Ok(key) => key,
        Err(response) => return response,
    };

    let offer_price_koinu =
        match marketplace_parse_koinu(&body.offer_price_koinu, "offerPriceKoinu") {
            Ok(value) => value,
            Err(response) => return response,
        };
    let expires_at = match marketplace_parse_timestamp(&body.expires_at, "expiresAt") {
        Ok(timestamp) => timestamp,
        Err(response) => return response,
    };

    if expires_at <= chrono::Utc::now() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_offer",
            "expiresAt must be in the future",
        );
    }

    if body.maker_address.trim().is_empty() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_offer",
            "makerAddress is required",
        );
    }

    let scope = body.scope.trim().to_ascii_lowercase();
    match scope.as_str() {
        "item"
            if body
                .inscription_id
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty() =>
        {
            return marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_offer",
                "inscriptionId is required for item offers",
            )
        }
        "collection"
            if body
                .collection_id
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty() =>
        {
            return marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_offer",
                "collectionId is required for collection offers",
            )
        }
        "item" | "collection" => {}
        _ => {
            return marketplace_error(
                StatusCode::BAD_REQUEST,
                "invalid_offer",
                "scope must be either 'item' or 'collection'",
            )
        }
    }

    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    match client
        .query_opt(
            "SELECT id, scope, inscription_id, collection_id, maker_address,
                    target_seller_address, offer_price_koinu, marketplace_fee_bps,
                    status, expires_at, created_at, updated_at
             FROM marketplace_offers
             WHERE idempotency_key = $1",
            &[&idempotency_key],
        )
        .await
    {
        Ok(Some(row)) => return Json(marketplace_offer_row(&row)).into_response(),
        Ok(None) => {}
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to check offer idempotency",
            )
        }
    }

    let payload = match marketplace_verify_signed_intent(
        &state,
        &client,
        &body.signed_intent,
        "offer_create",
        Some(&body.maker_address),
        true,
    )
    .await
    {
        Ok(payload) => payload,
        Err(response) => return response,
    };

    if payload
        .get("offerPriceKoinu")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value != body.offer_price_koinu)
    {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent offerPriceKoinu does not match the request body",
        );
    }
    if payload
        .get("scope")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value != scope)
    {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent scope does not match the request body",
        );
    }
    if payload
        .get("collectionId")
        .and_then(serde_json::Value::as_str)
        .zip(body.collection_id.as_deref())
        .is_some_and(|(intent_value, body_value)| intent_value != body_value)
    {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent collectionId does not match the request body",
        );
    }
    if payload
        .get("inscriptionId")
        .and_then(serde_json::Value::as_str)
        .zip(body.inscription_id.as_deref())
        .is_some_and(|(intent_value, body_value)| intent_value != body_value)
    {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent inscriptionId does not match the request body",
        );
    }

    if marketplace_upsert_trader(&client, &body.maker_address)
        .await
        .is_err()
    {
        return marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to prepare trader profile",
        );
    }

    let offer_id = marketplace_id("off");
    let now = marketplace_now();
    let expires_at_text = expires_at.to_rfc3339();
    let intent_payload = marketplace_metadata_text(body.intent_payload.as_ref());

    let row = match client
        .query_one(
            "INSERT INTO marketplace_offers (
                 id, scope, inscription_id, collection_id, maker_address,
                 target_seller_address, offer_price_koinu, marketplace_fee_bps,
                 status, expires_at, intent_payload, created_at, updated_at, idempotency_key
             ) VALUES (
                 $1, $2, $3, $4, $5,
                 $6, $7, $8,
                 'active', $9, $10, $11, $11, $12
             )
             RETURNING id, scope, inscription_id, collection_id, maker_address,
                       target_seller_address, offer_price_koinu, marketplace_fee_bps,
                       status, expires_at, created_at, updated_at",
            &[
                &offer_id,
                &scope,
                &body.inscription_id,
                &body.collection_id,
                &body.maker_address,
                &body.target_seller_address,
                &offer_price_koinu,
                &body.marketplace_fee_bps.unwrap_or(0),
                &expires_at_text,
                &intent_payload,
                &now,
                &idempotency_key,
            ],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to create marketplace offer",
            )
        }
    };

    let activity_metadata = json!({
        "scope": scope,
        "collectionId": body.collection_id,
        "targetSellerAddress": body.target_seller_address,
    });
    let _ = marketplace_insert_activity(
        &client,
        &body.maker_address,
        "offer_created",
        "offer",
        &offer_id,
        body.inscription_id.as_ref(),
        Some(offer_price_koinu),
        None,
        Some(&activity_metadata),
    )
    .await;

    (StatusCode::CREATED, Json(marketplace_offer_row(&row))).into_response()
}

pub async fn cancel_marketplace_offer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(offer_id): Path<String>,
    Json(body): Json<CancelMarketplaceOfferRequest>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    if let Err(response) = marketplace_require_session(&client, &headers, &body.maker_address).await
    {
        return response;
    }

    let existing = match client
        .query_opt(
            "SELECT maker_address, inscription_id, status
             FROM marketplace_offers
             WHERE id = $1",
            &[&offer_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return marketplace_error(
                StatusCode::NOT_FOUND,
                "offer_not_found",
                format!("Marketplace offer '{}' was not found", offer_id),
            )
        }
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to load offer for cancellation",
            )
        }
    };

    let maker_address: String = existing.get("maker_address");
    let inscription_id: Option<String> = existing.get("inscription_id");
    let status: String = existing.get("status");

    if maker_address != body.maker_address {
        return marketplace_error(
            StatusCode::FORBIDDEN,
            "offer_cancel_forbidden",
            "makerAddress does not match the offer owner",
        );
    }
    if status == "cancelled" {
        return marketplace_error(
            StatusCode::CONFLICT,
            "offer_already_cancelled",
            "Offer is already cancelled",
        );
    }
    if status != "active" {
        return marketplace_error(
            StatusCode::CONFLICT,
            "offer_not_cancellable",
            format!("Offer cannot be cancelled while in '{}' state", status),
        );
    }

    let now = marketplace_now();
    let row = match client
        .query_one(
            "UPDATE marketplace_offers
             SET status = 'cancelled', updated_at = $2
             WHERE id = $1
             RETURNING id, scope, inscription_id, collection_id, maker_address,
                       target_seller_address, offer_price_koinu, marketplace_fee_bps,
                       status, expires_at, created_at, updated_at",
            &[&offer_id, &now],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to cancel offer",
            )
        }
    };

    let _ = marketplace_insert_activity(
        &client,
        &maker_address,
        "offer_cancelled",
        "offer",
        &offer_id,
        inscription_id.as_ref(),
        None,
        None,
        None,
    )
    .await;

    Json(marketplace_offer_row(&row)).into_response()
}

pub async fn get_marketplace_auction(
    State(state): State<AppState>,
    Path(auction_id): Path<String>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    match client
        .query_opt(
            "SELECT id, inscription_id, seller_address, start_price_koinu,
                    reserve_price_koinu, min_increment_koinu, starts_at, ends_at,
                    status, highest_bid_id, highest_bidder_address,
                    highest_bid_amount_koinu, highest_bid_placed_at
             FROM marketplace_auctions
             WHERE id = $1",
            &[&auction_id],
        )
        .await
    {
        Ok(Some(row)) => {
            let bids = client
                .query(
                    "SELECT id, auction_id, bidder_address, bid_amount_koinu, status,
                            bidder_signature_payload_hash, bidder_signature,
                            bidder_signing_address, bidder_signed_at, created_at, updated_at
                     FROM marketplace_auction_bids
                     WHERE auction_id = $1
                     ORDER BY bid_amount_koinu DESC, created_at DESC",
                    &[&auction_id],
                )
                .await
                .map(|rows| rows.iter().map(marketplace_bid_row).collect::<Vec<_>>())
                .unwrap_or_default();

            let mut auction = marketplace_auction_row(&row);
            if let Some(object) = auction.as_object_mut() {
                object.insert("bids".to_string(), serde_json::Value::Array(bids));
            }
            Json(auction).into_response()
        }
        Ok(None) => marketplace_error(
            StatusCode::NOT_FOUND,
            "auction_not_found",
            format!("Marketplace auction '{}' was not found", auction_id),
        ),
        Err(_) => marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to load marketplace auction",
        ),
    }
}

pub async fn list_marketplace_auctions(
    State(state): State<AppState>,
    Query(params): Query<MarketplaceFeedParams>,
) -> Result<Json<MarketplaceListResponse<serde_json::Value>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let offset = marketplace_offset(&params);
    let min_price = marketplace_parse_optional_koinu(&params.min_price, "minPrice")
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let max_price = marketplace_parse_optional_koinu(&params.max_price, "maxPrice")
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let sort_sql = match params.sort.as_deref() {
        Some("price_asc") => "start_price_koinu ASC, created_at DESC",
        Some("price_desc") => "start_price_koinu DESC, created_at DESC",
        Some("ending_soon") => "ends_at ASC, created_at DESC",
        _ => "created_at DESC",
    };

    let count_row = client
        .query_one(
            "SELECT COUNT(*)::bigint
             FROM marketplace_auctions
             WHERE ($1::text IS NULL OR status = $1)
               AND ($2::text IS NULL OR seller_address = $2)
               AND ($3::text IS NULL OR inscription_id = $3)
               AND ($4::bigint IS NULL OR start_price_koinu >= $4)
               AND ($5::bigint IS NULL OR start_price_koinu <= $5)",
            &[
                &params.status,
                &params.seller_address,
                &params.inscription_id,
                &min_price,
                &max_price,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total: i64 = count_row.get(0);

    let query = format!(
        "SELECT id, inscription_id, seller_address, start_price_koinu,
                reserve_price_koinu, min_increment_koinu, starts_at, ends_at,
                status, highest_bid_id, highest_bidder_address,
                highest_bid_amount_koinu, highest_bid_placed_at
         FROM marketplace_auctions
         WHERE ($1::text IS NULL OR status = $1)
           AND ($2::text IS NULL OR seller_address = $2)
           AND ($3::text IS NULL OR inscription_id = $3)
           AND ($4::bigint IS NULL OR start_price_koinu >= $4)
           AND ($5::bigint IS NULL OR start_price_koinu <= $5)
         ORDER BY {}
         LIMIT $6 OFFSET $7",
        sort_sql
    );

    let rows = client
        .query(
            &query,
            &[
                &params.status,
                &params.seller_address,
                &params.inscription_id,
                &min_price,
                &max_price,
                &params.limit,
                &offset,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(MarketplaceListResponse {
        items: rows.iter().map(marketplace_auction_row).collect(),
        total: total.max(0) as usize,
        next_cursor: marketplace_next_cursor(offset, params.limit, total),
    }))
}

pub async fn create_marketplace_auction(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateMarketplaceAuctionRequest>,
) -> impl IntoResponse {
    let idempotency_key = match marketplace_idempotency_key(&headers) {
        Ok(key) => key,
        Err(response) => return response,
    };

    let start_price_koinu =
        match marketplace_parse_koinu(&body.start_price_koinu, "startPriceKoinu") {
            Ok(value) => value,
            Err(response) => return response,
        };
    let reserve_price_koinu =
        match marketplace_parse_optional_koinu(&body.reserve_price_koinu, "reservePriceKoinu") {
            Ok(value) => value,
            Err(response) => return response,
        };
    let min_increment_koinu =
        match marketplace_parse_koinu(&body.min_increment_koinu, "minIncrementKoinu") {
            Ok(value) => value,
            Err(response) => return response,
        };
    let starts_at = match marketplace_parse_timestamp(&body.starts_at, "startsAt") {
        Ok(timestamp) => timestamp,
        Err(response) => return response,
    };
    let ends_at = match marketplace_parse_timestamp(&body.ends_at, "endsAt") {
        Ok(timestamp) => timestamp,
        Err(response) => return response,
    };

    if body.seller_address.trim().is_empty() || body.inscription_id.trim().is_empty() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_auction",
            "sellerAddress and inscriptionId are required",
        );
    }
    if ends_at <= starts_at {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_auction",
            "endsAt must be later than startsAt",
        );
    }
    if ends_at <= chrono::Utc::now() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_auction",
            "endsAt must be in the future",
        );
    }
    if reserve_price_koinu.is_some_and(|reserve| reserve < start_price_koinu) {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_auction",
            "reservePriceKoinu cannot be lower than startPriceKoinu",
        );
    }

    let anti_sniping_window_sec = body.anti_sniping_window_sec.unwrap_or(0);
    let anti_sniping_extension_sec = body.anti_sniping_extension_sec.unwrap_or(0);
    if anti_sniping_window_sec < 0 || anti_sniping_extension_sec < 0 {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_auction",
            "Anti-sniping values must be zero or positive",
        );
    }

    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    if let Err(response) =
        marketplace_require_session(&client, &headers, &body.seller_address).await
    {
        return response;
    }

    match client
        .query_opt(
            "SELECT id, inscription_id, seller_address, start_price_koinu,
                    reserve_price_koinu, min_increment_koinu, starts_at, ends_at,
                    status, highest_bid_id, highest_bidder_address,
                    highest_bid_amount_koinu, highest_bid_placed_at
             FROM marketplace_auctions
             WHERE idempotency_key = $1",
            &[&idempotency_key],
        )
        .await
    {
        Ok(Some(row)) => return Json(marketplace_auction_row(&row)).into_response(),
        Ok(None) => {}
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to check auction idempotency",
            )
        }
    }

    if marketplace_upsert_trader(&client, &body.seller_address)
        .await
        .is_err()
    {
        return marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to prepare trader profile",
        );
    }

    let auction_id = marketplace_id("auc");
    let now = marketplace_now();
    let status = if starts_at <= chrono::Utc::now() {
        "live"
    } else {
        "scheduled"
    };
    let starts_at_text = starts_at.to_rfc3339();
    let ends_at_text = ends_at.to_rfc3339();

    let row = match client
        .query_one(
            "INSERT INTO marketplace_auctions (
                 id, inscription_id, seller_address, start_price_koinu,
                 reserve_price_koinu, min_increment_koinu, starts_at, ends_at,
                 status, anti_sniping_window_sec, anti_sniping_extension_sec,
                 created_at, updated_at, idempotency_key
             ) VALUES (
                 $1, $2, $3, $4,
                 $5, $6, $7, $8,
                 $9, $10, $11,
                 $12, $12, $13
             )
             RETURNING id, inscription_id, seller_address, start_price_koinu,
                       reserve_price_koinu, min_increment_koinu, starts_at, ends_at,
                       status, highest_bid_id, highest_bidder_address,
                       highest_bid_amount_koinu, highest_bid_placed_at",
            &[
                &auction_id,
                &body.inscription_id,
                &body.seller_address,
                &start_price_koinu,
                &reserve_price_koinu,
                &min_increment_koinu,
                &starts_at_text,
                &ends_at_text,
                &status,
                &anti_sniping_window_sec,
                &anti_sniping_extension_sec,
                &now,
                &idempotency_key,
            ],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to create marketplace auction",
            )
        }
    };

    let activity_metadata = json!({
        "reservePriceKoinu": reserve_price_koinu.map(|value| value.to_string()),
        "antiSnipingWindowSec": anti_sniping_window_sec,
        "antiSnipingExtensionSec": anti_sniping_extension_sec,
    });
    let _ = marketplace_insert_activity(
        &client,
        &body.seller_address,
        "auction_created",
        "auction",
        &auction_id,
        Some(&body.inscription_id),
        Some(start_price_koinu),
        None,
        Some(&activity_metadata),
    )
    .await;

    (StatusCode::CREATED, Json(marketplace_auction_row(&row))).into_response()
}

pub async fn create_marketplace_auction_bid(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(auction_id): Path<String>,
    Json(body): Json<CreateMarketplaceBidRequest>,
) -> impl IntoResponse {
    let idempotency_key = match marketplace_idempotency_key(&headers) {
        Ok(key) => key,
        Err(response) => return response,
    };
    let bid_amount_koinu = match marketplace_parse_koinu(&body.bid_amount_koinu, "bidAmountKoinu") {
        Ok(value) => value,
        Err(response) => return response,
    };

    if body.bidder_address.trim().is_empty() {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_bid",
            "bidderAddress is required",
        );
    }

    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    match client
        .query_opt(
            "SELECT id, auction_id, bidder_address, bid_amount_koinu, status,
                    bidder_signature_payload_hash, bidder_signature,
                    bidder_signing_address, bidder_signed_at, created_at, updated_at
             FROM marketplace_auction_bids
             WHERE idempotency_key = $1",
            &[&idempotency_key],
        )
        .await
    {
        Ok(Some(row)) => return Json(marketplace_bid_row(&row)).into_response(),
        Ok(None) => {}
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to check auction bid idempotency",
            )
        }
    }

    let payload = match marketplace_verify_signed_intent(
        &state,
        &client,
        &body.signed_intent,
        "bid_place",
        Some(&body.bidder_address),
        true,
    )
    .await
    {
        Ok(payload) => payload,
        Err(response) => return response,
    };

    if payload
        .get("auctionId")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value != auction_id)
    {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent auctionId does not match the request path",
        );
    }
    if payload
        .get("bidAmountKoinu")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value != body.bid_amount_koinu)
    {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent bidAmountKoinu does not match the request body",
        );
    }

    let auction = match client
        .query_opt(
            "SELECT id, inscription_id, seller_address, start_price_koinu,
                    reserve_price_koinu, min_increment_koinu, starts_at, ends_at,
                    status, highest_bid_id, highest_bidder_address,
                    highest_bid_amount_koinu, highest_bid_placed_at,
                    anti_sniping_window_sec, anti_sniping_extension_sec
             FROM marketplace_auctions
             WHERE id = $1",
            &[&auction_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return marketplace_error(
                StatusCode::NOT_FOUND,
                "auction_not_found",
                format!("Marketplace auction '{}' was not found", auction_id),
            )
        }
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to load marketplace auction",
            )
        }
    };

    let starts_at =
        match marketplace_parse_timestamp(&auction.get::<_, String>("starts_at"), "startsAt") {
            Ok(value) => value,
            Err(response) => return response,
        };
    let ends_at = match marketplace_parse_timestamp(&auction.get::<_, String>("ends_at"), "endsAt")
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let now_ts = chrono::Utc::now();
    if now_ts < starts_at {
        return marketplace_error(
            StatusCode::CONFLICT,
            "auction_not_live",
            "Auction has not started yet",
        );
    }
    if now_ts >= ends_at {
        return marketplace_error(
            StatusCode::CONFLICT,
            "auction_ended",
            "Auction has already ended",
        );
    }

    let status: String = auction.get("status");
    if matches!(status.as_str(), "settled" | "cancelled" | "invalidated") {
        return marketplace_error(
            StatusCode::CONFLICT,
            "auction_not_bidable",
            format!("Auction cannot accept bids while in '{}' state", status),
        );
    }

    let start_price_koinu: i64 = auction.get("start_price_koinu");
    let min_increment_koinu: i64 = auction.get("min_increment_koinu");
    let current_highest: Option<i64> = auction.get("highest_bid_amount_koinu");
    let minimum_bid = current_highest
        .map(|value| value + min_increment_koinu)
        .unwrap_or(start_price_koinu);
    if bid_amount_koinu < minimum_bid {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "bid_too_low",
            format!("Bid must be at least {} koinu", minimum_bid),
        );
    }

    if marketplace_upsert_trader(&client, &body.bidder_address)
        .await
        .is_err()
    {
        return marketplace_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            "Failed to prepare trader profile",
        );
    }

    let current_highest_bid_id: Option<String> = auction.get("highest_bid_id");
    if let Some(current_highest_bid_id) = current_highest_bid_id.as_ref() {
        let _ = client
            .execute(
                "UPDATE marketplace_auction_bids
                 SET status = 'outbid', updated_at = $2
                 WHERE id = $1 AND status = 'winning'",
                &[current_highest_bid_id, &marketplace_now()],
            )
            .await;
    }

    let anti_sniping_window_sec: i32 = auction.get("anti_sniping_window_sec");
    let anti_sniping_extension_sec: i32 = auction.get("anti_sniping_extension_sec");
    let extended_ends_at = if anti_sniping_window_sec > 0
        && anti_sniping_extension_sec > 0
        && (ends_at - now_ts).num_seconds() <= anti_sniping_window_sec as i64
    {
        ends_at + chrono::Duration::seconds(anti_sniping_extension_sec as i64)
    } else {
        ends_at
    };

    let bid_id = marketplace_id("bid");
    let now = marketplace_now();
    let row = match client
        .query_one(
            "INSERT INTO marketplace_auction_bids (
                 id, auction_id, bidder_address, bid_amount_koinu, status,
                 bidder_signature_payload_hash, bidder_signature,
                 bidder_signing_address, bidder_signed_at, created_at, updated_at, idempotency_key
             ) VALUES (
                 $1, $2, $3, $4, 'winning',
                 $5, $6,
                 $7, $8, $9, $9, $10
             )
             RETURNING id, auction_id, bidder_address, bid_amount_koinu, status,
                       bidder_signature_payload_hash, bidder_signature,
                       bidder_signing_address, bidder_signed_at, created_at, updated_at",
            &[
                &bid_id,
                &auction_id,
                &body.bidder_address,
                &bid_amount_koinu,
                &body.signed_intent.payload_hash,
                &body.signed_intent.signature,
                &body.signed_intent.signing_address,
                &body.signed_intent.signed_at,
                &now,
                &idempotency_key,
            ],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to create marketplace bid",
            )
        }
    };

    let _ = client
        .execute(
            "UPDATE marketplace_auctions
             SET status = 'live',
                 highest_bid_id = $2,
                 highest_bidder_address = $3,
                 highest_bid_amount_koinu = $4,
                 highest_bid_placed_at = $5,
                 ends_at = $6,
                 updated_at = $5
             WHERE id = $1",
            &[
                &auction_id,
                &bid_id,
                &body.bidder_address,
                &bid_amount_koinu,
                &now,
                &extended_ends_at.to_rfc3339(),
            ],
        )
        .await;

    let auction_inscription_id: String = auction.get("inscription_id");
    let activity_metadata = json!({
        "auctionId": auction_id,
        "extendedEndsAt": extended_ends_at.to_rfc3339(),
    });
    let _ = marketplace_insert_activity(
        &client,
        &body.bidder_address,
        "bid_placed",
        "auction",
        &auction_id,
        Some(&auction_inscription_id),
        Some(bid_amount_koinu),
        None,
        Some(&activity_metadata),
    )
    .await;

    Json(json!({
        "bid": marketplace_bid_row(&row),
        "auctionId": auction_id,
        "endsAt": extended_ends_at.to_rfc3339(),
    }))
    .into_response()
}

pub async fn cancel_marketplace_auction_bid(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((auction_id, bid_id)): Path<(String, String)>,
    Json(body): Json<CancelMarketplaceBidRequest>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    if let Err(response) =
        marketplace_require_session(&client, &headers, &body.bidder_address).await
    {
        return response;
    }

    let bid = match client
        .query_opt(
            "SELECT id, auction_id, bidder_address, bid_amount_koinu, status,
                    bidder_signature_payload_hash, bidder_signature,
                    bidder_signing_address, bidder_signed_at, created_at, updated_at
             FROM marketplace_auction_bids
             WHERE id = $1 AND auction_id = $2",
            &[&bid_id, &auction_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return marketplace_error(
                StatusCode::NOT_FOUND,
                "bid_not_found",
                "Marketplace bid was not found",
            )
        }
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to load marketplace bid",
            )
        }
    };

    let bidder_address: String = bid.get("bidder_address");
    if bidder_address != body.bidder_address {
        return marketplace_error(
            StatusCode::FORBIDDEN,
            "bid_cancel_forbidden",
            "bidderAddress does not match the bid owner",
        );
    }

    let bid_status: String = bid.get("status");
    if !matches!(bid_status.as_str(), "winning" | "active" | "outbid") {
        return marketplace_error(
            StatusCode::CONFLICT,
            "bid_not_cancellable",
            format!("Bid cannot be cancelled while in '{}' state", bid_status),
        );
    }

    let now = marketplace_now();
    let row = match client
        .query_one(
            "UPDATE marketplace_auction_bids
             SET status = 'withdrawn', updated_at = $3
             WHERE id = $1 AND auction_id = $2
             RETURNING id, auction_id, bidder_address, bid_amount_koinu, status,
                       bidder_signature_payload_hash, bidder_signature,
                       bidder_signing_address, bidder_signed_at, created_at, updated_at",
            &[&bid_id, &auction_id, &now],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to cancel marketplace bid",
            )
        }
    };

    let replacement = client
        .query_opt(
            "SELECT id, bidder_address, bid_amount_koinu, created_at
             FROM marketplace_auction_bids
             WHERE auction_id = $1 AND status IN ('winning', 'outbid')
             ORDER BY bid_amount_koinu DESC, created_at DESC
             LIMIT 1",
            &[&auction_id],
        )
        .await
        .ok()
        .flatten();

    if let Some(replacement) = replacement {
        let replacement_id: String = replacement.get("id");
        let replacement_bidder_address: String = replacement.get("bidder_address");
        let replacement_bid_amount_koinu: i64 = replacement.get("bid_amount_koinu");
        let replacement_created_at: String = replacement.get("created_at");

        let _ = client
            .execute(
                "UPDATE marketplace_auction_bids
                 SET status = 'winning', updated_at = $2
                 WHERE id = $1",
                &[&replacement_id, &now],
            )
            .await;
        let _ = client
            .execute(
                "UPDATE marketplace_auctions
                 SET highest_bid_id = $2,
                     highest_bidder_address = $3,
                     highest_bid_amount_koinu = $4,
                     highest_bid_placed_at = $5,
                     updated_at = $6
                 WHERE id = $1",
                &[
                    &auction_id,
                    &replacement_id,
                    &replacement_bidder_address,
                    &replacement_bid_amount_koinu,
                    &replacement_created_at,
                    &now,
                ],
            )
            .await;
    } else {
        let _ = client
            .execute(
                "UPDATE marketplace_auctions
                 SET highest_bid_id = NULL,
                     highest_bidder_address = NULL,
                     highest_bid_amount_koinu = NULL,
                     highest_bid_placed_at = NULL,
                     updated_at = $2
                 WHERE id = $1",
                &[&auction_id, &now],
            )
            .await;
    }

    let activity_metadata = json!({ "bidId": bid_id });
    let _ = marketplace_insert_activity(
        &client,
        &body.bidder_address,
        "bid_withdrawn",
        "auction",
        &auction_id,
        None,
        None,
        None,
        Some(&activity_metadata),
    )
    .await;

    Json(marketplace_bid_row(&row)).into_response()
}

pub async fn settle_marketplace_auction(
    State(state): State<AppState>,
    Path(auction_id): Path<String>,
    Json(body): Json<SettleMarketplaceAuctionRequest>,
) -> impl IntoResponse {
    let client = match state.doginals_pool.get().await {
        Ok(client) => client,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_unavailable",
                "Marketplace database unavailable",
            )
        }
    };

    let payload = match marketplace_verify_signed_intent(
        &state,
        &client,
        &body.signed_intent,
        "auction_settle",
        Some(&body.seller_address),
        true,
    )
    .await
    {
        Ok(payload) => payload,
        Err(response) => return response,
    };

    if payload
        .get("auctionId")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value != auction_id)
    {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "invalid_intent",
            "Intent auctionId does not match the request path",
        );
    }

    let auction = match client
        .query_opt(
            "SELECT id, inscription_id, seller_address, ends_at, status,
                    highest_bid_id, highest_bid_amount_koinu
             FROM marketplace_auctions
             WHERE id = $1",
            &[&auction_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return marketplace_error(
                StatusCode::NOT_FOUND,
                "auction_not_found",
                format!("Marketplace auction '{}' was not found", auction_id),
            )
        }
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to load marketplace auction",
            )
        }
    };

    let seller_address: String = auction.get("seller_address");
    if seller_address != body.seller_address {
        return marketplace_error(
            StatusCode::FORBIDDEN,
            "auction_settle_forbidden",
            "sellerAddress does not match the auction owner",
        );
    }

    let ends_at = match marketplace_parse_timestamp(&auction.get::<_, String>("ends_at"), "endsAt")
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    if chrono::Utc::now() < ends_at {
        return marketplace_error(
            StatusCode::CONFLICT,
            "auction_not_ended",
            "Auction cannot be settled before it has ended",
        );
    }

    let highest_bid_id: Option<String> = auction.get("highest_bid_id");
    let highest_bid_amount_koinu: Option<i64> = auction.get("highest_bid_amount_koinu");
    let highest_bid_id = match highest_bid_id {
        Some(highest_bid_id) => highest_bid_id,
        None => {
            return marketplace_error(
                StatusCode::CONFLICT,
                "auction_no_bids",
                "Auction has no winning bid to settle",
            )
        }
    };
    let highest_bid_amount_koinu = highest_bid_amount_koinu.unwrap_or(0);

    let transaction = match marketplace_decode_raw_transaction(&body.signed_psbt) {
        Ok(transaction) => transaction,
        Err(response) => return response,
    };
    let seller_script = match marketplace_parse_dogecoin_address(&seller_address) {
        Ok(script) => script,
        Err(response) => return response,
    };
    let seller_paid_koinu = marketplace_output_value_for_script(&transaction, &seller_script);
    if seller_paid_koinu < highest_bid_amount_koinu {
        return marketplace_error(
            StatusCode::BAD_REQUEST,
            "insufficient_payment",
            format!(
                "Transaction pays {} koinu to seller but auction requires {}",
                seller_paid_koinu, highest_bid_amount_koinu
            ),
        );
    }

    let txid = match marketplace_broadcast_raw_transaction(&state, &body.signed_psbt).await {
        Ok(txid) => txid,
        Err(response) => return response,
    };
    let now = marketplace_now();
    let txid_string = txid.clone();
    let auction_inscription_id: String = auction.get("inscription_id");
    let settle_metadata = json!({ "bidId": highest_bid_id });

    let row = match client
        .query_one(
            "UPDATE marketplace_auctions
             SET status = 'settled', updated_at = $2
             WHERE id = $1
             RETURNING id, inscription_id, seller_address, start_price_koinu,
                       reserve_price_koinu, min_increment_koinu, starts_at, ends_at,
                       status, highest_bid_id, highest_bidder_address,
                       highest_bid_amount_koinu, highest_bid_placed_at",
            &[&auction_id, &now],
        )
        .await
    {
        Ok(row) => row,
        Err(_) => {
            return marketplace_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                "Failed to settle marketplace auction",
            )
        }
    };

    let _ = client
        .execute(
            "UPDATE marketplace_auction_bids
             SET status = 'settled', settlement_txid = $3, updated_at = $4
             WHERE id = $1 AND auction_id = $2",
            &[&highest_bid_id, &auction_id, &txid_string, &now],
        )
        .await;

    let _ = marketplace_insert_activity(
        &client,
        &seller_address,
        "auction_settled",
        "auction",
        &auction_id,
        Some(&auction_inscription_id),
        Some(highest_bid_amount_koinu),
        Some(&txid_string),
        Some(&settle_metadata),
    )
    .await;

    Json(json!({
        "auction": marketplace_auction_row(&row),
        "txid": txid_string,
        "status": "settled",
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// DMP handlers
// ---------------------------------------------------------------------------

/// Query params for GET /api/dmp/listings
#[derive(Deserialize)]
pub struct DmpListingsParams {
    #[serde(default = "default_dmp_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_dmp_limit() -> i64 {
    50
}

/// Response shape for a single DMP listing.
#[derive(Serialize)]
pub struct DmpListingResponse {
    pub listing_id: String,
    pub seller: String,
    pub price_koinu: i64,
    pub psbt_cid: String,
    pub expiry_height: i64,
    pub block_height: i64,
    pub block_timestamp: i64,
}

/// `GET /api/dmp/listings` — active (non-cancelled, non-settled) DMP listings.
pub async fn get_dmp_listings(
    State(state): State<AppState>,
    Query(params): Query<DmpListingsParams>,
) -> Result<Json<Vec<DmpListingResponse>>, StatusCode> {
    let client = state
        .doginals_pool
        .get()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = client
        .query(
            "SELECT listing_id, seller, price_koinu, psbt_cid, expiry_height,
                    block_height, block_timestamp
             FROM dmp_listings
             WHERE NOT cancelled AND NOT settled
             ORDER BY block_height DESC, listing_id ASC
             LIMIT $1 OFFSET $2",
            &[&params.limit, &params.offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let listings: Vec<DmpListingResponse> = rows
        .iter()
        .map(|r| DmpListingResponse {
            listing_id: r.get(0),
            seller: r.get(1),
            price_koinu: r.get(2),
            psbt_cid: r.get(3),
            expiry_height: r.get(4),
            block_height: r.get(5),
            block_timestamp: r.get(6),
        })
        .collect();

    Ok(Json(listings))
}
