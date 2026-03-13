//! DMP — inscription-based marketplace for Doginals.
//!
//! Every listing, bid, settlement, and cancel is an on-chain inscription whose
//! body is a JSON object with `"protocol": "DMP"` and `"version": "1.0"`.
//!
//! PSBTs live entirely off-chain (IPFS or Arweave); only the CID is inscribed.
//! The indexer stores the inscription-level activity; PSBT validation is out of scope.
//!
//! # Wire format (inscription body)
//! ```json
//! {
//!   "protocol": "DMP",
//!   "version":  "1.0",
//!   "op":       "listing" | "bid" | "settle" | "cancel",
//!   "listing_id":   "<inscription_id of the original listing>",  // bids/settles/cancels only
//!   "bid_id":       "<inscription_id of the accepted bid>",       // settle only, optional
//!   "seller":       "D...",
//!   "price_koinu":  4206900000,
//!   "psbt_cid":     "ipfs://Qm... or ar://...",
//!   "expiry_height": 5000000,
//!   "nonce":        12345,
//!   "signature":    "hex sig of the above fields"
//! }
//! ```

use serde::Deserialize;

use crate::manifest::expand_json_keys;

/// Wire-format protocol identifier.
pub const DMP_PROTOCOL: &str = "DMP";
/// Wire-format version string.
pub const DMP_VERSION: &str = "1.0";

// ---------------------------------------------------------------------------
// Parsed operation variants
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmpListing {
    /// Inscription ID of this listing (set by the indexer after parsing).
    pub inscription_id: String,
    pub seller: String,
    pub price_koinu: u64,
    /// ipfs://Qm... or ar://... pointing to the unsigned PSBT
    pub psbt_cid: String,
    pub expiry_height: u64,
    pub nonce: u64,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmpBid {
    pub inscription_id: String,
    /// Inscription ID of the listing this bid targets.
    pub listing_id: String,
    /// In the spec the bidder address is carried in the `seller` field.
    pub bidder: String,
    pub price_koinu: u64,
    pub psbt_cid: String,
    pub expiry_height: u64,
    pub nonce: u64,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmpSettle {
    pub inscription_id: String,
    pub listing_id: String,
    /// Which bid was accepted (optional).
    pub bid_id: Option<String>,
    /// Address that broadcast the settle inscription.
    pub settler: String,
    pub psbt_cid: String,
    pub nonce: u64,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmpCancel {
    pub inscription_id: String,
    pub listing_id: String,
    pub canceller: String,
    pub nonce: u64,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmpOperation {
    Listing(DmpListing),
    Bid(DmpBid),
    Settle(DmpSettle),
    Cancel(DmpCancel),
}

// ---------------------------------------------------------------------------
// Raw serde deserialisation helper
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawDmp {
    #[serde(alias = "p")]
    protocol: Option<String>,
    #[serde(alias = "v")]
    version: Option<String>,
    op: Option<String>,
    #[serde(alias = "lid")]
    listing_id: Option<String>,
    #[serde(alias = "bid")]
    bid_id: Option<String>,
    #[serde(alias = "s")]
    seller: Option<String>,
    #[serde(alias = "pk")]
    price_koinu: Option<u64>,
    #[serde(alias = "pc")]
    psbt_cid: Option<String>,
    #[serde(alias = "ex")]
    expiry_height: Option<u64>,
    #[serde(alias = "n")]
    nonce: Option<u64>,
    #[serde(alias = "sig")]
    signature: Option<String>,
}

// ---------------------------------------------------------------------------
// Public parse entry-point
// ---------------------------------------------------------------------------

/// Attempt to parse a DMP inscription body.
///
/// Returns `None` if the body is not a valid DMP inscription (wrong protocol,
/// missing required fields, or malformed values).
///
/// The `inscription_id` of the containing inscription must be supplied by the
/// caller so we can embed it in the returned struct.
pub fn try_parse_dmp(body: &[u8], inscription_id: &str) -> Option<DmpOperation> {
    let text = std::str::from_utf8(body).ok()?.trim();
    if text.is_empty() {
        return None;
    }

    let raw: RawDmp = serde_json::from_slice(&expand_json_keys(text.as_bytes()).unwrap_or_else(|| text.as_bytes().to_vec())).ok()?;

    if raw.protocol.as_deref()? != DMP_PROTOCOL {
        return None;
    }
    // version must be present and "1.0"
    if raw.version.as_deref()? != DMP_VERSION {
        return None;
    }

    match raw.op.as_deref()? {
        "listing" => parse_listing(raw, inscription_id),
        "bid" => parse_bid(raw, inscription_id),
        "settle" => parse_settle(raw, inscription_id),
        "cancel" => parse_cancel(raw, inscription_id),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Per-op parsers
// ---------------------------------------------------------------------------

fn parse_listing(raw: RawDmp, inscription_id: &str) -> Option<DmpOperation> {
    let seller = normalize_address(raw.seller?)?;
    let price_koinu = raw.price_koinu?;
    let psbt_cid = normalize_cid(raw.psbt_cid?)?;
    let expiry_height = raw.expiry_height?;
    let nonce = raw.nonce?;
    let signature = normalize_hex_sig(raw.signature?)?;

    if price_koinu == 0 || expiry_height == 0 {
        return None;
    }

    Some(DmpOperation::Listing(DmpListing {
        inscription_id: inscription_id.to_string(),
        seller,
        price_koinu,
        psbt_cid,
        expiry_height,
        nonce,
        signature,
    }))
}

fn parse_bid(raw: RawDmp, inscription_id: &str) -> Option<DmpOperation> {
    let listing_id = normalize_inscription_id(raw.listing_id?)?;
    let bidder = normalize_address(raw.seller?)?;
    let price_koinu = raw.price_koinu?;
    let psbt_cid = normalize_cid(raw.psbt_cid?)?;
    let expiry_height = raw.expiry_height?;
    let nonce = raw.nonce?;
    let signature = normalize_hex_sig(raw.signature?)?;

    if price_koinu == 0 || expiry_height == 0 {
        return None;
    }

    Some(DmpOperation::Bid(DmpBid {
        inscription_id: inscription_id.to_string(),
        listing_id,
        bidder,
        price_koinu,
        psbt_cid,
        expiry_height,
        nonce,
        signature,
    }))
}

fn parse_settle(raw: RawDmp, inscription_id: &str) -> Option<DmpOperation> {
    let listing_id = normalize_inscription_id(raw.listing_id?)?;
    let bid_id = raw.bid_id.and_then(|id| normalize_inscription_id(id));
    let settler = normalize_address(raw.seller?)?;
    let psbt_cid = normalize_cid(raw.psbt_cid?)?;
    let nonce = raw.nonce?;
    let signature = normalize_hex_sig(raw.signature?)?;

    Some(DmpOperation::Settle(DmpSettle {
        inscription_id: inscription_id.to_string(),
        listing_id,
        bid_id,
        settler,
        psbt_cid,
        nonce,
        signature,
    }))
}

fn parse_cancel(raw: RawDmp, inscription_id: &str) -> Option<DmpOperation> {
    let listing_id = normalize_inscription_id(raw.listing_id?)?;
    let canceller = normalize_address(raw.seller?)?;
    let nonce = raw.nonce?;
    let signature = normalize_hex_sig(raw.signature?)?;

    Some(DmpOperation::Cancel(DmpCancel {
        inscription_id: inscription_id.to_string(),
        listing_id,
        canceller,
        nonce,
        signature,
    }))
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn normalize_address(addr: String) -> Option<String> {
    let addr = addr.trim();
    if addr.len() < 24 || addr.len() > 128 {
        return None;
    }
    if addr
        .chars()
        .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
    {
        return None;
    }
    Some(addr.to_string())
}

fn normalize_cid(cid: String) -> Option<String> {
    let cid = cid.trim();
    if cid.len() < 7 || cid.len() > 256 {
        return None;
    }
    // Must start with a known off-chain scheme
    if !cid.starts_with("ipfs://") && !cid.starts_with("ar://") {
        return None;
    }
    Some(cid.to_string())
}

fn normalize_inscription_id(id: String) -> Option<String> {
    let id = id.trim();
    // inscription IDs are "<txid>i<index>", e.g. "abc...i0" (min ~66 chars)
    if id.len() < 64 || id.len() > 128 {
        return None;
    }
    Some(id.to_string())
}

fn normalize_hex_sig(sig: String) -> Option<String> {
    let sig = sig.trim();
    if sig.is_empty() || sig.len() > 512 {
        return None;
    }
    // Must be valid hex
    if !sig.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(sig.to_string())
}
