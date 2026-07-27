-- ABOUTME: Binds generation metadata to the credential-selected routing profile.
-- ABOUTME: Keeps historical rows production-scoped while preventing cross-profile lookup.

ALTER TABLE generations
    ADD COLUMN routing_profile TEXT NOT NULL DEFAULT 'production',
    ADD CONSTRAINT generations_routing_profile_check
        CHECK (routing_profile IN ('production', 'beta'));
