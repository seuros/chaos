-- Repair process metadata created before persisted role=user response items
-- participated in the process projection.
-- This is a repair, not session activity, so preserve updated_at ordering.
ALTER TABLE processes DISABLE TRIGGER processes_touch;

WITH raw_messages AS (
    SELECT
        p.id,
        COALESCE(
            (
                SELECT je.payload_json #>> '{payload,message}'
                FROM journal_entries AS je
                WHERE je.process_id = p.id
                  AND je.item_type = 'event_msg'
                  AND je.payload_json ->> 'type' = 'event_msg'
                  AND je.payload_json #>> '{payload,type}' = 'user_message'
                  AND btrim(je.payload_json #>> '{payload,message}') <> ''
                ORDER BY je.seq ASC
                LIMIT 1
            ),
            (
                SELECT content.value ->> 'text'
                FROM journal_entries AS je
                CROSS JOIN LATERAL jsonb_array_elements(
                    CASE
                        WHEN jsonb_typeof(je.payload_json #> '{payload,content}') = 'array'
                        THEN je.payload_json #> '{payload,content}'
                        ELSE '[]'::jsonb
                    END
                ) WITH ORDINALITY AS content(value, position)
                WHERE je.process_id = p.id
                  AND je.item_type = 'response_item'
                  AND je.payload_json ->> 'type' = 'response_item'
                  AND je.payload_json #>> '{payload,type}' = 'message'
                  AND je.payload_json #>> '{payload,role}' = 'user'
                  AND content.value ->> 'type' = 'input_text'
                  AND btrim(content.value ->> 'text') <> ''
                ORDER BY je.seq ASC, content.position ASC
                LIMIT 1
            )
        ) AS message
    FROM processes AS p
    WHERE btrim(p.first_user_message) = ''
       OR btrim(p.title) = ''
),
cleaned_messages AS (
    SELECT
        id,
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
    FROM raw_messages
    WHERE message IS NOT NULL
)
UPDATE processes AS p
SET
    first_user_message = CASE
        WHEN btrim(p.first_user_message) = '' THEN cleaned.message
        ELSE p.first_user_message
    END,
    title = CASE
        WHEN btrim(p.title) = '' THEN cleaned.message
        ELSE p.title
    END
FROM cleaned_messages AS cleaned
WHERE p.id = cleaned.id
  AND cleaned.message <> '';

ALTER TABLE processes ENABLE TRIGGER processes_touch;
