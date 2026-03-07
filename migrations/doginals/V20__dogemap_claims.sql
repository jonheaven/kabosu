CREATE TABLE IF NOT EXISTS dogemap_claims (
    block_number        BIGINT  NOT NULL,
    inscription_id      TEXT    NOT NULL,
    claim_height        BIGINT  NOT NULL,
    claim_timestamp     BIGINT  NOT NULL,
    PRIMARY KEY (block_number)
);

CREATE INDEX IF NOT EXISTS dogemap_claims_claim_height_idx ON dogemap_claims (claim_height);
