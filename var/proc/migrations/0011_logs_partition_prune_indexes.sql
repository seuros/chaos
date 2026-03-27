CREATE INDEX idx_logs_process_id_ts ON logs(process_id, ts DESC, ts_nanos DESC, id DESC);

CREATE INDEX idx_logs_process_uuid_processless_ts ON logs(process_uuid, ts DESC, ts_nanos DESC, id DESC)
WHERE process_id IS NULL;
