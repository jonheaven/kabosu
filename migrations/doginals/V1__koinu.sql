CREATE TABLE koinu (
    doginal_number NUMERIC NOT NULL PRIMARY KEY,
    rarity TEXT NOT NULL,
    coinbase_height NUMERIC NOT NULL
);
CREATE INDEX koinu_rarity_index ON koinu (rarity);
