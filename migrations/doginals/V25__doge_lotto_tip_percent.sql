ALTER TABLE lotto_tickets
    ADD COLUMN IF NOT EXISTS tip_percent INTEGER NOT NULL DEFAULT 0;

ALTER TABLE lotto_winners
    ADD COLUMN IF NOT EXISTS gross_payout_koinu BIGINT,
    ADD COLUMN IF NOT EXISTS tip_percent INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS tip_deduction_koinu BIGINT;

UPDATE lotto_winners
SET gross_payout_koinu = COALESCE(gross_payout_koinu, payout_koinu),
    tip_deduction_koinu = COALESCE(tip_deduction_koinu, 0);

ALTER TABLE lotto_winners
    ALTER COLUMN gross_payout_koinu SET NOT NULL,
    ALTER COLUMN tip_deduction_koinu SET NOT NULL;
