-- Same delivery record as SQLite: pending, claimed, confirmed or dead, with the
-- latest attempt identity and lease end kept after settlement and a 32-attempt
-- limit. Legacy rows keep their attempt count, so archived evidence is unchanged.
ALTER TABLE execution_outbox ADD COLUMN delivery_state TEXT NOT NULL DEFAULT 'pending'
    CHECK (delivery_state IN ('pending', 'claimed', 'confirmed', 'dead'));
ALTER TABLE execution_outbox ADD COLUMN failed_after_ms BIGINT;
ALTER TABLE execution_outbox ADD COLUMN observed_at_ms BIGINT NOT NULL DEFAULT 0;
ALTER TABLE execution_outbox ADD COLUMN dead_at_ms BIGINT;
ALTER TABLE execution_outbox ADD COLUMN dead_reason TEXT;

UPDATE execution_outbox SET
    delivery_state = CASE
        WHEN delivered_at_ms IS NOT NULL THEN 'confirmed'
        WHEN attempts = 0 THEN 'pending'
        WHEN attempts <= 32 AND COALESCE(lease_token, 0) = GREATEST(last_issued, attempts)
            AND expires_at_ms > next_attempt_at_ms THEN 'claimed'
        WHEN attempts >= 32 THEN 'dead'
        ELSE 'pending'
    END,
    lease_token = CASE WHEN attempts = 0 THEN NULL ELSE GREATEST(last_issued, attempts) END,
    last_issued = GREATEST(last_issued, attempts),
    expires_at_ms = CASE
        WHEN delivered_at_ms IS NOT NULL
            THEN GREATEST(expires_at_ms, delivered_at_ms, next_attempt_at_ms + 1)
        WHEN attempts = 0 THEN 0
        WHEN attempts <= 32 AND COALESCE(lease_token, 0) = GREATEST(last_issued, attempts)
            AND expires_at_ms > next_attempt_at_ms THEN expires_at_ms
        WHEN attempts >= 32 THEN GREATEST(expires_at_ms, next_attempt_at_ms + 1)
        WHEN expires_at_ms > 0 THEN expires_at_ms
        ELSE next_attempt_at_ms
    END,
    next_attempt_at_ms = CASE
        WHEN delivered_at_ms IS NULL AND attempts BETWEEN 1 AND 31
            AND NOT (COALESCE(lease_token, 0) = GREATEST(last_issued, attempts)
                AND expires_at_ms > next_attempt_at_ms)
            THEN GREATEST(next_attempt_at_ms, expires_at_ms)
        ELSE next_attempt_at_ms
    END,
    observed_at_ms = GREATEST(created_at_ms, next_attempt_at_ms, COALESCE(delivered_at_ms, 0),
        CASE
            WHEN delivered_at_ms IS NULL AND attempts BETWEEN 1 AND 31
                AND NOT (COALESCE(lease_token, 0) = GREATEST(last_issued, attempts)
                    AND expires_at_ms > next_attempt_at_ms)
                THEN expires_at_ms
            ELSE 0
        END),
    dead_at_ms = CASE
        WHEN delivered_at_ms IS NULL AND attempts >= 32
            AND NOT (attempts = 32 AND COALESCE(lease_token, 0) = GREATEST(last_issued, attempts)
                AND expires_at_ms > next_attempt_at_ms)
            THEN CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)
    END,
    dead_reason = CASE
        WHEN delivered_at_ms IS NULL AND attempts >= 32
            AND NOT (attempts = 32 AND COALESCE(lease_token, 0) = GREATEST(last_issued, attempts)
                AND expires_at_ms > next_attempt_at_ms)
            THEN 'attempts exhausted'
    END;

DROP INDEX execution_outbox_due;
CREATE INDEX execution_outbox_due ON execution_outbox(scope, delivery_state, next_attempt_at_ms);
