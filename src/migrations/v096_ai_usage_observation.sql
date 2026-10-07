-- Preserve historical counters/costs without inventing their completeness.
ALTER TABLE ai_usage_events ADD COLUMN usage_status TEXT NOT NULL DEFAULT 'legacy_unverified'
    CHECK (usage_status IN ('complete', 'partial', 'missing', 'invalid', 'estimated', 'legacy_unverified'));
ALTER TABLE ai_usage_events ADD COLUMN attempt_outcome TEXT NOT NULL DEFAULT 'unknown'
    CHECK (attempt_outcome IN ('success', 'failed', 'unknown'));
ALTER TABLE ai_usage_events ADD COLUMN cost_status TEXT NOT NULL DEFAULT 'legacy_unverified'
    CHECK (cost_status IN ('complete', 'partial', 'unpriced', 'legacy_unverified'));
ALTER TABLE ai_usage_events ADD COLUMN usage_details_json TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(usage_details_json) AND json_type(usage_details_json) = 'object');
