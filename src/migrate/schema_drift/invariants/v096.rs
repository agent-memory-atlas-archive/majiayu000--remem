use super::SchemaInvariant;

pub(in crate::migrate) const V096_SCHEMA_INVARIANTS: &[SchemaInvariant] = &[
    SchemaInvariant::column(
        96,
        "ai_usage_observation",
        "ai_usage_events",
        "usage_status",
    ),
    SchemaInvariant::column(
        96,
        "ai_usage_observation",
        "ai_usage_events",
        "attempt_outcome",
    ),
    SchemaInvariant::column(96, "ai_usage_observation", "ai_usage_events", "cost_status"),
    SchemaInvariant::column(
        96,
        "ai_usage_observation",
        "ai_usage_events",
        "usage_details_json",
    ),
];
