-- Successful chunk progress is distinct from the cursor, which historical
-- exhaustion may advance over failed evidence. Never backfill from that cursor.
ALTER TABLE extraction_tasks ADD COLUMN completed_event_id INTEGER
    CHECK (completed_event_id IS NULL OR completed_event_id > 0);
