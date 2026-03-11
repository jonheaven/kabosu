CREATE TABLE IF NOT EXISTS marketplace_traders (
  address TEXT PRIMARY KEY,
  display_name TEXT,
  bio TEXT,
  avatar_url TEXT,
  x_handle TEXT,
  x_user_id TEXT,
  x_verified BOOLEAN NOT NULL DEFAULT FALSE,
  x_verified_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS marketplace_activity (
  id BIGSERIAL PRIMARY KEY,
  trader_address TEXT NOT NULL,
  event_type TEXT NOT NULL,
  entity_type TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  inscription_id TEXT,
  amount_koinu BIGINT,
  txid TEXT,
  metadata TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS marketplace_activity_trader_created_idx
  ON marketplace_activity (trader_address, created_at DESC);

CREATE TABLE IF NOT EXISTS marketplace_listings (
  id TEXT PRIMARY KEY,
  inscription_id TEXT NOT NULL,
  collection_id TEXT,
  seller_address TEXT NOT NULL,
  asking_price_koinu BIGINT NOT NULL,
  currency TEXT NOT NULL DEFAULT 'DOGE',
  marketplace_fee_bps INTEGER NOT NULL DEFAULT 0,
  royalty_bps INTEGER,
  status TEXT NOT NULL DEFAULT 'active',
  expiry_at TEXT,
  seller_signed_template TEXT,
  settlement_txid TEXT,
  settlement_block_height BIGINT,
  settlement_confirmations INTEGER NOT NULL DEFAULT 0,
  finalized_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE
);

CREATE INDEX IF NOT EXISTS marketplace_listings_status_created_idx
  ON marketplace_listings (status, created_at DESC);
CREATE INDEX IF NOT EXISTS marketplace_listings_seller_created_idx
  ON marketplace_listings (seller_address, created_at DESC);
CREATE INDEX IF NOT EXISTS marketplace_listings_inscription_idx
  ON marketplace_listings (inscription_id);

CREATE TABLE IF NOT EXISTS marketplace_offers (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,
  inscription_id TEXT,
  collection_id TEXT,
  maker_address TEXT NOT NULL,
  target_seller_address TEXT,
  offer_price_koinu BIGINT NOT NULL,
  marketplace_fee_bps INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'active',
  expires_at TEXT NOT NULL,
  intent_payload TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE
);

CREATE INDEX IF NOT EXISTS marketplace_offers_status_created_idx
  ON marketplace_offers (status, created_at DESC);
CREATE INDEX IF NOT EXISTS marketplace_offers_maker_created_idx
  ON marketplace_offers (maker_address, created_at DESC);

CREATE TABLE IF NOT EXISTS marketplace_auctions (
  id TEXT PRIMARY KEY,
  inscription_id TEXT NOT NULL,
  seller_address TEXT NOT NULL,
  start_price_koinu BIGINT NOT NULL,
  reserve_price_koinu BIGINT,
  min_increment_koinu BIGINT NOT NULL,
  starts_at TEXT NOT NULL,
  ends_at TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'scheduled',
  highest_bid_id TEXT,
  highest_bidder_address TEXT,
  highest_bid_amount_koinu BIGINT,
  highest_bid_placed_at TEXT,
  anti_sniping_window_sec INTEGER NOT NULL DEFAULT 0,
  anti_sniping_extension_sec INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE
);

CREATE INDEX IF NOT EXISTS marketplace_auctions_status_created_idx
  ON marketplace_auctions (status, created_at DESC);
CREATE INDEX IF NOT EXISTS marketplace_auctions_seller_created_idx
  ON marketplace_auctions (seller_address, created_at DESC);
