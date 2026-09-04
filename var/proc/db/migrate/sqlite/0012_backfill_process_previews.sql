-- Repair process metadata created before persisted role=user response items
-- participated in the process projection.
-- This is a repair, not session activity, so preserve updated_at ordering.
DROP TRIGGER IF EXISTS processes_touch;

WITH RECURSIVE
raw_messages AS (
    SELECT
        p.id,
        COALESCE(
            (
                SELECT json_extract(je.payload_json, '$.payload.message')
                FROM journal_entries AS je
                WHERE je.process_id = p.id
                  AND je.item_type = 'event_msg'
                  AND json_extract(je.payload_json, '$.type') = 'event_msg'
                  AND json_extract(je.payload_json, '$.payload.type') = 'user_message'
                  AND trim(json_extract(je.payload_json, '$.payload.message')) <> ''
                ORDER BY je.seq ASC
                LIMIT 1
            ),
            (
                SELECT json_extract(content.value, '$.text')
                FROM journal_entries AS je
                JOIN json_each(je.payload_json, '$.payload.content') AS content
                WHERE je.process_id = p.id
                  AND je.item_type = 'response_item'
                  AND json_extract(je.payload_json, '$.type') = 'response_item'
                  AND json_extract(je.payload_json, '$.payload.type') = 'message'
                  AND json_extract(je.payload_json, '$.payload.role') = 'user'
                  AND json_extract(content.value, '$.type') = 'input_text'
                  AND trim(json_extract(content.value, '$.text')) <> ''
                ORDER BY je.seq ASC, CAST(content.key AS INTEGER) ASC
                LIMIT 1
            )
        ) AS message
    FROM processes AS p
    WHERE trim(p.first_user_message) = ''
       OR trim(p.title) = ''
),
stripped_environment(id, message, depth) AS (
    SELECT id, ltrim(message, char(9) || char(10) || char(13) || ' '), 0
    FROM raw_messages
    WHERE message IS NOT NULL

    UNION ALL

    SELECT
        id,
        ltrim(
            substr(
                message,
                instr(message, '</environment_context>')
                    + length('</environment_context>')
            ),
            char(9) || char(10) || char(13) || ' '
        ),
        depth + 1
    FROM stripped_environment
    WHERE depth < 8
      AND substr(message, 1, length('<environment_context>')) = '<environment_context>'
      AND instr(message, '</environment_context>') > 0
),
environment_free AS (
    SELECT stripped.id, stripped.message
    FROM stripped_environment AS stripped
    WHERE stripped.depth = (
        SELECT max(candidate.depth)
        FROM stripped_environment AS candidate
        WHERE candidate.id = stripped.id
    )
),
cleaned_messages AS (
    SELECT
        id,
        trim(
            CASE
                WHEN ltrim(message, char(9) || char(10) || char(13) || ' ')
                    LIKE '## My request for %:%'
                THEN substr(
                    ltrim(message, char(9) || char(10) || char(13) || ' '),
                    instr(
                        ltrim(message, char(9) || char(10) || char(13) || ' '),
                        ':'
                    ) + 1
                )
                ELSE message
            END
        ) AS message
    FROM environment_free
)
UPDATE processes
SET
    first_user_message = CASE
        WHEN trim(first_user_message) = ''
        THEN COALESCE(
            (SELECT message FROM cleaned_messages WHERE cleaned_messages.id = processes.id),
            first_user_message
        )
        ELSE first_user_message
    END,
    title = CASE
        WHEN trim(title) = ''
        THEN COALESCE(
            (SELECT message FROM cleaned_messages WHERE cleaned_messages.id = processes.id),
            title
        )
        ELSE title
    END
WHERE id IN (
    SELECT id
    FROM cleaned_messages
    WHERE message <> ''
);

CREATE TRIGGER processes_touch
AFTER UPDATE ON processes
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE processes SET updated_at = UNIXEPOCH() WHERE id = NEW.id;
END;
