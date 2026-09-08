-- One-time journal format upgrade. Stop old writers before upgrading.
-- SQLx runs this migration in a transaction, including the append-only guard.
DROP TRIGGER journal_entries_no_update;

UPDATE journal_entries AS turn
SET payload_json = json_set(
    turn.payload_json,
    '$.payload.model_provider',
    COALESCE(
        (
            SELECT json_extract(meta.payload_json, '$.payload.model_provider')
            FROM journal_entries AS meta
            WHERE meta.process_id = turn.process_id
              AND meta.seq <= turn.seq
              AND meta.item_type = 'session_meta'
              AND json_type(meta.payload_json, '$.payload.model_provider') = 'text'
              AND trim(json_extract(meta.payload_json, '$.payload.model_provider')) <> ''
            ORDER BY meta.seq DESC
            LIMIT 1
        ),
        (SELECT p.model_provider FROM processes AS p WHERE p.id = turn.process_id)
    )
)
WHERE turn.item_type = 'turn_context'
  AND (
      json_type(turn.payload_json, '$.payload.model_provider') IS NULL
      OR json_type(turn.payload_json, '$.payload.model_provider') = 'null'
  );

CREATE TRIGGER journal_entries_no_update
BEFORE UPDATE ON journal_entries
FOR EACH ROW
BEGIN
    SELECT RAISE(FAIL, 'journal_entries is append-only');
END;
