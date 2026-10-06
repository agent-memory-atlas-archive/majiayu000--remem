use super::SchemaInvariant;

pub(in crate::migrate) const V095_SCHEMA_INVARIANTS: &[SchemaInvariant] = &[
    SchemaInvariant::table(95, "worker_fair_dispatch", "worker_dispatch_state"),
    SchemaInvariant::column(
        95,
        "worker_fair_dispatch",
        "worker_dispatch_state",
        "ready_sequence",
    ),
    SchemaInvariant::column(
        95,
        "worker_fair_dispatch",
        "worker_dispatch_state",
        "last_claim_sequence",
    ),
    SchemaInvariant::index(95, "worker_fair_dispatch", "idx_worker_dispatch_last_claim"),
];
