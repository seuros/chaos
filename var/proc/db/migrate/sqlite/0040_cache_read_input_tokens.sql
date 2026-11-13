-- One-time journal format upgrade. Stop old writers before upgrading.
-- SQLx runs this migration in a transaction, including the append-only guard.
DROP TRIGGER journal_entries_no_update;

UPDATE journal_entries
SET payload_json = json_set(
    json_remove(
        payload_json,
        '$.payload.info.total_token_usage.cached_input_tokens',
        '$.payload.info.last_token_usage.cached_input_tokens'
    ),
    '$.payload.info.total_token_usage.cache_read_input_tokens',
    json_extract(payload_json, '$.payload.info.total_token_usage.cached_input_tokens'),
    '$.payload.info.last_token_usage.cache_read_input_tokens',
    json_extract(payload_json, '$.payload.info.last_token_usage.cached_input_tokens')
)
WHERE item_type = 'event_msg'
  AND json_extract(payload_json, '$.payload.type') = 'token_count'
  AND json_type(payload_json, '$.payload.info.total_token_usage.cached_input_tokens') IS NOT NULL
  AND json_type(payload_json, '$.payload.info.last_token_usage.cached_input_tokens') IS NOT NULL;

CREATE TRIGGER journal_entries_no_update
BEFORE UPDATE ON journal_entries
FOR EACH ROW
BEGIN
    SELECT RAISE(FAIL, 'journal_entries is append-only');
END;
