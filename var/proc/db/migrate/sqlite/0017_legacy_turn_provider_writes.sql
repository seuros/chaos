-- Repair turns appended by old writers after 0016, then normalize future
-- legacy inserts in the same transaction. Never use a runtime/global default.
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

-- SQLite cannot assign NEW fields. Insert the normalized row instead, then
-- suppress the original insert. No UPDATE exception to append-only is needed.
DROP TRIGGER IF EXISTS journal_entries_legacy_turn_provider;
CREATE TRIGGER journal_entries_legacy_turn_provider
BEFORE INSERT ON journal_entries
FOR EACH ROW
WHEN NEW.item_type = 'turn_context'
  AND (
      json_type(NEW.payload_json, '$.payload.model_provider') IS NULL
      OR json_type(NEW.payload_json, '$.payload.model_provider') = 'null'
  )
BEGIN
    SELECT CASE WHEN
        json_type(NEW.payload_json, '$.payload') IS NOT 'object'
        OR NOT EXISTS (SELECT 1 FROM processes WHERE id = NEW.process_id)
    THEN RAISE(ABORT, 'cannot recover legacy turn model_provider; upgrade the journal writer')
    END;

    INSERT INTO journal_entries (process_id, seq, recorded_at, item_type, payload_json)
    VALUES (
        NEW.process_id, NEW.seq, NEW.recorded_at, NEW.item_type,
        json_set(
            NEW.payload_json,
            '$.payload.model_provider',
            COALESCE(
                (
                    SELECT json_extract(meta.payload_json, '$.payload.model_provider')
                    FROM journal_entries AS meta
                    WHERE meta.process_id = NEW.process_id
                      AND meta.seq <= NEW.seq
                      AND meta.item_type = 'session_meta'
                      AND json_type(meta.payload_json, '$.payload.model_provider') = 'text'
                      AND trim(json_extract(meta.payload_json, '$.payload.model_provider')) <> ''
                    ORDER BY meta.seq DESC
                    LIMIT 1
                ),
                (SELECT p.model_provider FROM processes AS p WHERE p.id = NEW.process_id)
            )
        )
    );
    SELECT RAISE(IGNORE);
END;
