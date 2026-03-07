-- Ranvier Audit Events Table
-- Used by PostgresAuditSink for tamper-proof audit logging.

CREATE TABLE IF NOT EXISTS audit_events (
    id          TEXT PRIMARY KEY,
    timestamp   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    actor       TEXT NOT NULL,
    action      TEXT NOT NULL,
    target      TEXT NOT NULL,
    intent      TEXT,
    metadata    JSONB NOT NULL DEFAULT '{}',
    prev_hash   TEXT
);

-- Index for common query patterns
CREATE INDEX IF NOT EXISTS idx_audit_events_actor     ON audit_events (actor);
CREATE INDEX IF NOT EXISTS idx_audit_events_action    ON audit_events (action);
CREATE INDEX IF NOT EXISTS idx_audit_events_target    ON audit_events (target);
CREATE INDEX IF NOT EXISTS idx_audit_events_timestamp ON audit_events (timestamp);
