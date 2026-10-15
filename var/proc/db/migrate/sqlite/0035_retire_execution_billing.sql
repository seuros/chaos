-- Preserve migration 33 and its recorded checksum for existing installations.
-- Financial services now belong to PostgreSQL. Never discard existing evidence:
-- a populated installation must export/migrate it explicitly before retiring it.
-- SQLx runs this entire step in one transaction, including the guard.
CREATE TEMP TABLE execution_financial_retirement_guard (
    evidence_rows INTEGER NOT NULL,
    CONSTRAINT export_financial_evidence_before_retiring_sqlite_billing
        CHECK (evidence_rows = 0)
);
INSERT INTO execution_financial_retirement_guard
SELECT (SELECT COUNT(*) FROM execution_price_schedules)
     + (SELECT COUNT(*) FROM execution_reconciliations);
DROP TABLE execution_reconciliations;
DROP TABLE execution_price_schedules;
DROP TABLE execution_financial_retirement_guard;
