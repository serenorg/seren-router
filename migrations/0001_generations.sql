-- ABOUTME: Creates the durable provider-generation ledger used for reconciliation.
-- ABOUTME: Stores exact Decimal costs and the provider response ID used by /generation.

CREATE TABLE generations (
    id TEXT PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    canonical_slug TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    prompt_tokens BIGINT,
    completion_tokens BIGINT,
    cost_usd NUMERIC(18, 10),
    latency_ms BIGINT NOT NULL,
    status SMALLINT NOT NULL
);
