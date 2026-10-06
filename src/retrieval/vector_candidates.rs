use super::vector::VectorSearchFilters;

pub const VECTOR_SEARCH_CANDIDATE_LIMIT: usize = 4_096;
const VECTOR_SEARCH_MIN_CANDIDATES: usize = 512;

pub(crate) fn vector_candidate_limit(limit: usize) -> usize {
    limit.clamp(VECTOR_SEARCH_MIN_CANDIDATES, VECTOR_SEARCH_CANDIDATE_LIMIT)
}

pub(crate) fn memory_filter_conditions(
    filters: VectorSearchFilters<'_>,
    start_idx: usize,
) -> (Vec<String>, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conditions = vec![crate::memory::memory_current_filter_sql(
        "m.status",
        "m.expires_at_epoch",
        filters.include_stale,
    )];
    if !filters.include_stale {
        conditions.push(crate::memory::memory_state_key_current_filter_sql("m"));
    }
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = start_idx;
    if let Some(project) = filters.project {
        conditions.push(format!("(m.project = ?{idx} OR m.scope = 'global')"));
        values.push(Box::new(project.to_string()));
        idx += 1;
    }
    if let Some(branch) = filters.branch {
        conditions.push(format!("(m.branch = ?{idx} OR m.branch IS NULL)"));
        values.push(Box::new(branch.to_string()));
        idx += 1;
    }
    if let Some(memory_type) = filters.memory_type {
        conditions.push(format!("m.memory_type = ?{idx}"));
        values.push(Box::new(memory_type.to_string()));
    }
    (conditions, values)
}
