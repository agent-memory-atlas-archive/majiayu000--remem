use rusqlite::types::ToSql;

use super::VectorSearchFilters;

/// Eligibility assembled by trusted Rust callers, never caller-supplied SQL.
/// Both executors apply it before nearest-neighbor selection.
pub(crate) struct VectorSearchScope {
    pub(super) conditions: Vec<String>,
    pub(super) values: Vec<Box<dyn ToSql>>,
}

impl VectorSearchScope {
    pub(crate) fn from_memory_predicates(
        conditions: Vec<String>,
        values: Vec<Box<dyn ToSql>>,
    ) -> Self {
        Self { conditions, values }
    }

    pub(super) fn from_filters(filters: VectorSearchFilters<'_>) -> Self {
        let (conditions, values) =
            super::super::vector_candidates::memory_filter_conditions(filters, 1);
        Self::from_memory_predicates(conditions, values)
    }

    pub(super) fn predicate(&self) -> String {
        if self.conditions.is_empty() {
            "1 = 1".to_string()
        } else {
            self.conditions.join(" AND ")
        }
    }

    pub(super) fn eligible_ids_sql(&self, model_idx: usize, dimensions_idx: usize) -> String {
        format!(
            "SELECT e.memory_id FROM memory_embeddings e
             JOIN memories m ON m.id = e.memory_id
             WHERE e.model = ?{model_idx} AND e.dimensions = ?{dimensions_idx}
               AND {}",
            self.predicate()
        )
    }
}
