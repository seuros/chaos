CREATE UNIQUE INDEX processes_attested_review_attempt_unique
ON processes(parent_process_id, agent_role)
WHERE parent_process_id IS NOT NULL
  AND agent_role LIKE '__chaos_internal__:attested-review:%';
