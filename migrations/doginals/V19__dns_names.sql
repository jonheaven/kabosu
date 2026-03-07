CREATE TABLE IF NOT EXISTS dns_names (
    name            TEXT        NOT NULL,
    inscription_id  TEXT        NOT NULL,
    block_height    BIGINT      NOT NULL,
    block_timestamp BIGINT      NOT NULL,
    PRIMARY KEY (name)
);

CREATE INDEX IF NOT EXISTS dns_names_block_height_idx ON dns_names (block_height);
CREATE INDEX IF NOT EXISTS dns_names_namespace_idx    ON dns_names (split_part(name, '.', 2));
