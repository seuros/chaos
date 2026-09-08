-- Repair turns appended by old writers after 0016, then normalize future
-- legacy inserts in the same transaction. Never use a runtime/global default.
SET LOCAL lock_timeout = '5s';
DROP TRIGGER journal_entries_no_update ON journal_entries;

-- Share the recovery policy between the backfill and future legacy inserts.
CREATE OR REPLACE FUNCTION chaos_legacy_turn_provider(target_process_id text, target_seq bigint)
RETURNS text LANGUAGE sql VOLATILE AS $$
    SELECT COALESCE(
        (
            SELECT meta.payload_json #>> '{payload,model_provider}'
            FROM journal_entries AS meta
            WHERE meta.process_id = target_process_id
              AND meta.seq <= target_seq
              AND meta.item_type = 'session_meta'
              AND jsonb_typeof(meta.payload_json #> '{payload,model_provider}') = 'string'
              AND btrim(meta.payload_json #>> '{payload,model_provider}') <> ''
            ORDER BY meta.seq DESC
            LIMIT 1
        ),
        (SELECT p.model_provider FROM processes AS p WHERE p.id = target_process_id)
    );
$$;

UPDATE journal_entries AS turn
SET payload_json = jsonb_set(
    turn.payload_json,
    '{payload,model_provider}',
    to_jsonb(chaos_legacy_turn_provider(turn.process_id, turn.seq))
)
WHERE turn.item_type = 'turn_context'
  AND (
      turn.payload_json #> '{payload,model_provider}' IS NULL
      OR turn.payload_json #> '{payload,model_provider}' = 'null'::jsonb
  );

CREATE TRIGGER journal_entries_no_update
BEFORE UPDATE ON journal_entries
FOR EACH ROW
EXECUTE FUNCTION chaos_raise_append_only();

CREATE OR REPLACE FUNCTION chaos_fill_legacy_turn_provider() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    provider text;
BEGIN
    provider := chaos_legacy_turn_provider(NEW.process_id, NEW.seq);

    IF provider IS NULL OR
       jsonb_typeof(NEW.payload_json -> 'payload') IS DISTINCT FROM 'object' THEN
        RAISE EXCEPTION 'cannot recover legacy turn model_provider; upgrade the journal writer';
    END IF;
    NEW.payload_json := jsonb_set(
        NEW.payload_json, '{payload,model_provider}', to_jsonb(provider)
    );
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS journal_entries_legacy_turn_provider ON journal_entries;
CREATE TRIGGER journal_entries_legacy_turn_provider
BEFORE INSERT ON journal_entries
FOR EACH ROW
WHEN (
    NEW.item_type = 'turn_context'
    AND (
        NEW.payload_json #> '{payload,model_provider}' IS NULL
        OR NEW.payload_json #> '{payload,model_provider}' = 'null'::jsonb
    )
)
EXECUTE FUNCTION chaos_fill_legacy_turn_provider();
