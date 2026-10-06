use super::SchemaInvariant;

pub(in crate::migrate) const V094_SCHEMA_INVARIANTS: &[SchemaInvariant] =
    &[SchemaInvariant::column(
        94,
        "extraction_completed_progress",
        "extraction_tasks",
        "completed_event_id",
    )];
