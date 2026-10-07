-- Operational claim order only. No task/job history or source rows are changed.
CREATE TABLE worker_dispatch_state (
    scope TEXT NOT NULL CHECK (scope IN ('queue', 'extraction', 'job')),
    stage TEXT NOT NULL,
    host TEXT NOT NULL,
    project TEXT NOT NULL,
    ready_sequence INTEGER NOT NULL CHECK (ready_sequence >= 0),
    last_claim_sequence INTEGER
        CHECK (last_claim_sequence IS NULL OR last_claim_sequence >= ready_sequence),
    PRIMARY KEY (scope, stage, host, project)
);
CREATE INDEX idx_worker_dispatch_last_claim ON worker_dispatch_state(last_claim_sequence);
