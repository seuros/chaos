-- Migration 12 selected the first raw user item before removing injected
-- context. When that item contained only <environment_context>, cleanup made
-- it empty and the next real user message was never considered. Clean every
-- candidate first, then select the earliest non-empty result.
-- This is a repair, not session activity, so preserve updated_at ordering.
-- A transaction-local flag avoids ALTER TABLE ... DISABLE TRIGGER, which needs
-- an exclusive table lock and can deadlock with live journal writers.
CREATE OR REPLACE FUNCTION chaos_touch_updated_at_epoch()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.updated_at = OLD.updated_at
       AND COALESCE(current_setting('chaos.preserve_updated_at', true), 'off') <> 'on'
    THEN
        NEW.updated_at := EXTRACT(EPOCH FROM clock_timestamp())::BIGINT;
    END IF;
    RETURN NEW;
END;
$$;

SET LOCAL chaos.preserve_updated_at = 'on';

WITH candidate_messages AS (
    SELECT
        p.id,
        je.seq,
        0::BIGINT AS position,
        je.payload_json #>> '{payload,message}' AS message
    FROM processes AS p
    JOIN journal_entries AS je ON je.process_id = p.id
    WHERE (btrim(p.first_user_message) = '' OR btrim(p.title) = '')
      AND je.item_type = 'event_msg'
      AND je.payload_json ->> 'type' = 'event_msg'
      AND je.payload_json #>> '{payload,type}' = 'user_message'
      AND btrim(je.payload_json #>> '{payload,message}') <> ''

    UNION ALL

    SELECT
        p.id,
        je.seq,
        content.position,
        content.value ->> 'text' AS message
    FROM processes AS p
    JOIN journal_entries AS je ON je.process_id = p.id
    CROSS JOIN LATERAL jsonb_array_elements(
        CASE
            WHEN jsonb_typeof(je.payload_json #> '{payload,content}') = 'array'
            THEN je.payload_json #> '{payload,content}'
            ELSE '[]'::jsonb
        END
    ) WITH ORDINALITY AS content(value, position)
    WHERE (btrim(p.first_user_message) = '' OR btrim(p.title) = '')
      AND je.item_type = 'response_item'
      AND je.payload_json ->> 'type' = 'response_item'
      AND je.payload_json #>> '{payload,type}' = 'message'
      AND je.payload_json #>> '{payload,role}' = 'user'
      AND content.value ->> 'type' = 'input_text'
      AND btrim(content.value ->> 'text') <> ''
),
cleaned_candidates AS (
    SELECT
        id,
        seq,
        position,
        btrim(
            regexp_replace(
                regexp_replace(
                    message,
                    '^\s*(<environment_context>.*</environment_context>\s*)+',
                    '',
                    's'
                ),
                '^\s*## My request for [^:]+:\s*',
                ''
            )
        ) AS message
    FROM candidate_messages
),
first_messages AS (
    SELECT DISTINCT ON (id) id, message
    FROM cleaned_candidates
    WHERE message <> ''
    ORDER BY id, seq ASC, position ASC
)
UPDATE processes AS p
SET
    first_user_message = CASE
        WHEN btrim(p.first_user_message) = '' THEN first_messages.message
        ELSE p.first_user_message
    END,
    title = CASE
        WHEN btrim(p.title) = '' THEN first_messages.message
        ELSE p.title
    END
FROM first_messages
WHERE p.id = first_messages.id;
