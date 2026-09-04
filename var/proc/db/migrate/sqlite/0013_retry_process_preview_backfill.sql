-- Migration 12 selected the first raw user item before removing injected
-- context. When that item contained only <environment_context>, cleanup made
-- it empty and the next real user message was never considered. Clean every
-- candidate first, then select the earliest non-empty result.
-- This is a repair, not session activity, so preserve updated_at ordering.
DROP TRIGGER IF EXISTS processes_touch;

WITH RECURSIVE
candidate_messages(id, seq, position, message) AS (
    SELECT
        p.id,
        je.seq,
        0,
        json_extract(je.payload_json, '$.payload.message')
    FROM processes AS p
    JOIN journal_entries AS je ON je.process_id = p.id
    WHERE (trim(p.first_user_message) = '' OR trim(p.title) = '')
      AND je.item_type = 'event_msg'
      AND json_extract(je.payload_json, '$.type') = 'event_msg'
      AND json_extract(je.payload_json, '$.payload.type') = 'user_message'
      AND trim(json_extract(je.payload_json, '$.payload.message')) <> ''

    UNION ALL

    SELECT
        p.id,
        je.seq,
        CAST(content.key AS INTEGER),
        json_extract(content.value, '$.text')
    FROM processes AS p
    JOIN journal_entries AS je ON je.process_id = p.id
    JOIN json_each(je.payload_json, '$.payload.content') AS content
    WHERE (trim(p.first_user_message) = '' OR trim(p.title) = '')
      AND je.item_type = 'response_item'
      AND json_extract(je.payload_json, '$.type') = 'response_item'
      AND json_extract(je.payload_json, '$.payload.type') = 'message'
      AND json_extract(je.payload_json, '$.payload.role') = 'user'
      AND json_extract(content.value, '$.type') = 'input_text'
      AND trim(json_extract(content.value, '$.text')) <> ''
),
stripped_environment(id, seq, position, message, depth) AS (
    SELECT
        id,
        seq,
        position,
        ltrim(message, char(9) || char(10) || char(13) || ' '),
        0
    FROM candidate_messages

    UNION ALL

    SELECT
        id,
        seq,
        position,
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
    SELECT
        stripped.id,
        stripped.seq,
        stripped.position,
        stripped.message
    FROM stripped_environment AS stripped
    WHERE stripped.depth = (
        SELECT max(candidate.depth)
        FROM stripped_environment AS candidate
        WHERE candidate.id = stripped.id
          AND candidate.seq = stripped.seq
          AND candidate.position = stripped.position
    )
),
cleaned_candidates AS (
    SELECT
        id,
        seq,
        position,
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
            (
                SELECT message
                FROM cleaned_candidates
                WHERE cleaned_candidates.id = processes.id
                  AND message <> ''
                ORDER BY seq ASC, position ASC
                LIMIT 1
            ),
            first_user_message
        )
        ELSE first_user_message
    END,
    title = CASE
        WHEN trim(title) = ''
        THEN COALESCE(
            (
                SELECT message
                FROM cleaned_candidates
                WHERE cleaned_candidates.id = processes.id
                  AND message <> ''
                ORDER BY seq ASC, position ASC
                LIMIT 1
            ),
            title
        )
        ELSE title
    END
WHERE EXISTS (
    SELECT 1
    FROM cleaned_candidates
    WHERE cleaned_candidates.id = processes.id
      AND message <> ''
);

CREATE TRIGGER processes_touch
AFTER UPDATE ON processes
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE processes SET updated_at = UNIXEPOCH() WHERE id = NEW.id;
END;
