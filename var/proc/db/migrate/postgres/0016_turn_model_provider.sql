-- One-time journal format upgrade. Stop old writers before upgrading.
-- Acquire the DDL lock before touching rows; fail promptly if writers are busy.
-- SQLx runs this migration in a transaction, including the append-only guard.
SET LOCAL lock_timeout = '5s';
DROP TRIGGER journal_entries_no_update ON journal_entries;

UPDATE journal_entries AS turn
SET payload_json = jsonb_set(
    turn.payload_json,
    '{payload,model_provider}',
    to_jsonb(COALESCE(
        (
            SELECT meta.payload_json #>> '{payload,model_provider}'
            FROM journal_entries AS meta
            WHERE meta.process_id = turn.process_id
              AND meta.seq <= turn.seq
              AND meta.item_type = 'session_meta'
              AND jsonb_typeof(meta.payload_json #> '{payload,model_provider}') = 'string'
              AND btrim(meta.payload_json #>> '{payload,model_provider}') <> ''
            ORDER BY meta.seq DESC
            LIMIT 1
        ),
        p.model_provider
    ))
)
FROM processes AS p
WHERE p.id = turn.process_id
  AND turn.item_type = 'turn_context'
  AND (
      turn.payload_json #> '{payload,model_provider}' IS NULL
      OR turn.payload_json #> '{payload,model_provider}' = 'null'::jsonb
  );

CREATE TRIGGER journal_entries_no_update
BEFORE UPDATE ON journal_entries
FOR EACH ROW
EXECUTE FUNCTION chaos_raise_append_only();
