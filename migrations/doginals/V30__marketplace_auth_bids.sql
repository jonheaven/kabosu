CREATE TABLE IF NOT EXISTS marketplace_auth_challenges (
  id TEXT PRIMARY KEY,
  address TEXT NOT NULL,
  challenge_token TEXT NOT NULL UNIQUE,
  message TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  used_at TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS marketplace_auth_challenges_address_expires_idx
  ON marketplace_auth_challenges (address, expires_at DESC);

CREATE TABLE IF NOT EXISTS marketplace_sessions (
  token TEXT PRIMARY KEY,
  address TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  revoked_at TEXT,
  created_at TEXT NOT NULL,
  last_used_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS marketplace_sessions_address_expires_idx
  ON marketplace_sessions (address, expires_at DESC);

CREATE TABLE IF NOT EXISTS marketplace_intent_nonces (
  nonce TEXT PRIMARY KEY,
  address TEXT NOT NULL,
  intent_type TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  consumed_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS marketplace_intent_nonces_address_consumed_idx
  ON marketplace_intent_nonces (address, consumed_at DESC);

CREATE TABLE IF NOT EXISTS marketplace_auction_bids (
  id TEXT PRIMARY KEY,
  auction_id TEXT NOT NULL,
  bidder_address TEXT NOT NULL,
  bid_amount_koinu BIGINT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active',
  bidder_signature_payload_hash TEXT,
  bidder_signature TEXT,
  bidder_signing_address TEXT,
  bidder_signed_at TEXT,
  settlement_txid TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE
);

CREATE INDEX IF NOT EXISTS marketplace_auction_bids_auction_created_idx
  ON marketplace_auction_bids (auction_id, created_at DESC);
CREATE INDEX IF NOT EXISTS marketplace_auction_bids_auction_status_created_idx
  ON marketplace_auction_bids (auction_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS marketplace_auction_bids_bidder_created_idx
  ON marketplace_auction_bids (bidder_address, created_at DESC);
