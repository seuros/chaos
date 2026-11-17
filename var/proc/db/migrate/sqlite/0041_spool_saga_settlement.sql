UPDATE spool_jobs
SET saga_json = json_set(
    saga_json,
    '$.settled',
    json_object(
        'direction', 'forward',
        'step', json_extract(saga_json, '$.step') - 1,
        'attempt', json_extract(saga_json, '$.attempt'),
        'outcome', 'success'
    )
)
WHERE saga_json IS NOT NULL
  AND json_extract(saga_json, '$.direction') = 'forward'
  AND json_extract(saga_json, '$.step') > 0
  AND json_type(saga_json, '$.settled') IS NULL;

UPDATE spool_jobs
SET status = 'Failed',
    error = 'submit saga was interrupted while compensating, and its saved state predates '
        || 'settlement tracking, so it cannot resume'
        || CASE
            WHEN batch_id IS NULL THEN ''
            ELSE '; cancel batch ' || batch_id || ' by hand if it is still running'
        END,
    completed_at = CAST(strftime('%s', 'now') AS INTEGER),
    updated_at = CAST(strftime('%s', 'now') AS INTEGER),
    saga_json = NULL
WHERE saga_json IS NOT NULL
  AND json_extract(saga_json, '$.direction') = 'compensating'
  AND json_type(saga_json, '$.settled') IS NULL;
