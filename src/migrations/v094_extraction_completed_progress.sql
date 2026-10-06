-- Successful chunk progress is distinct from the cursor, which historical
-- exhaustion may advance over failed evidence. Never backfill from that cursor.
ALTER TABLE extraction_tasks ADD COLUMN completed_event_id INTEGER
    CHECK (completed_event_id IS NULL OR completed_event_id > 0);

-- Replay members need an immutable original lower bound, independent of both
-- exhaustion cursors and successful progress. Legacy bounds are verified lazily.
ALTER TABLE extraction_tasks ADD COLUMN replay_from_event_id INTEGER
    CHECK (replay_from_event_id IS NULL OR replay_from_event_id > 0);
