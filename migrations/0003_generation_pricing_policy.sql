-- ABOUTME: Separates provider cost from the reviewed customer sell subtotal.
-- ABOUTME: Keeps cost_usd populated during mixed-version rollout and rollback.

ALTER TABLE generations
    ADD COLUMN provider_cost_usd NUMERIC(18, 10),
    ADD COLUMN sell_price_usd NUMERIC(18, 10),
    ADD CONSTRAINT generations_provider_cost_nonnegative
        CHECK (provider_cost_usd IS NULL OR provider_cost_usd >= 0),
    ADD CONSTRAINT generations_sell_price_nonnegative
        CHECK (sell_price_usd IS NULL OR sell_price_usd >= 0),
    ADD CONSTRAINT generations_legacy_cost_matches_sell_price
        CHECK (
            cost_usd IS NULL
            OR sell_price_usd IS NULL
            OR cost_usd = sell_price_usd
        );

UPDATE generations
SET
    provider_cost_usd = cost_usd,
    sell_price_usd = cost_usd
WHERE cost_usd IS NOT NULL;
