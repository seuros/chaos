UPDATE spool_jobs
SET saga_json = jsonb_set(
    saga_json::jsonb,
    '{settled}',
    jsonb_build_object(
        'direction', 'forward',
        'step', (saga_json::jsonb ->> 'step')::BIGINT - 1,
        'attempt', saga_json::jsonb -> 'attempt',
        'outcome', 'success'
    )
)::TEXT
WHERE saga_json IS NOT NULL
  AND saga_json::jsonb ->> 'direction' = 'forward'
  AND (saga_json::jsonb ->> 'step')::BIGINT > 0
  AND saga_json::jsonb -> 'settled' IS NULL;

UPDATE spool_jobs
SET status = 'Failed',
    error = 'submit saga was interrupted while compensating, and its saved state predates '
        || 'settlement tracking, so it cannot resume'
        || CASE
            WHEN batch_id IS NULL THEN ''
            ELSE '; cancel batch ' || batch_id || ' by hand if it is still running'
        END,
    completed_at = EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    updated_at = EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    saga_json = NULL
WHERE saga_json IS NOT NULL
  AND saga_json::jsonb ->> 'direction' = 'compensating'
  AND saga_json::jsonb -> 'settled' IS NULL;
