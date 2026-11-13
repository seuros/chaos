-- One-time journal format upgrade. Stop old writers before upgrading.
-- Acquire the DDL lock before touching rows; fail promptly if writers are busy.
-- SQLx runs this migration in a transaction, including the append-only guard.
SET LOCAL lock_timeout = '5s';
DROP TRIGGER journal_entries_no_update ON journal_entries;

UPDATE journal_entries
SET payload_json = jsonb_set(
    jsonb_set(
        payload_json
            #- '{payload,info,total_token_usage,cached_input_tokens}'
            #- '{payload,info,last_token_usage,cached_input_tokens}',
        '{payload,info,total_token_usage,cache_read_input_tokens}',
        payload_json #> '{payload,info,total_token_usage,cached_input_tokens}'
    ),
    '{payload,info,last_token_usage,cache_read_input_tokens}',
    payload_json #> '{payload,info,last_token_usage,cached_input_tokens}'
)
WHERE item_type = 'event_msg'
  AND payload_json #>> '{payload,type}' = 'token_count'
  AND payload_json #> '{payload,info,total_token_usage}' ? 'cached_input_tokens'
  AND payload_json #> '{payload,info,last_token_usage}' ? 'cached_input_tokens';

CREATE TRIGGER journal_entries_no_update
BEFORE UPDATE ON journal_entries
FOR EACH ROW
EXECUTE FUNCTION chaos_raise_append_only();
