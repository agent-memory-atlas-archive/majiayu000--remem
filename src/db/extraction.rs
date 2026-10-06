use crate::db::ExtractionTaskKind;

mod enqueue;
mod exhaust;
mod input;
mod lifecycle;
mod loaders;
mod progress;

pub use enqueue::*;
pub(crate) use input::{
    bound_captured_event_attempt, extraction_input_bytes, extraction_prompt_fits,
    log_extraction_input, CAPTURED_EVENT_BATCH_LIMIT, EXTRACTION_INPUT_MAX_BYTES,
    EXTRACTION_WRAPPER_RESERVE_BYTES,
};
pub use lifecycle::*;
pub use progress::mark_extraction_task_done;
pub(crate) use progress::{checkpoint_claimed_extraction_task_chunk, replay_resume_event_id};

pub const EXTRACTION_TASK_MAX_ATTEMPTS: i64 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionTask {
    pub id: i64,
    pub task_kind: ExtractionTaskKind,
    pub host_id: i64,
    pub workspace_id: i64,
    pub project_id: i64,
    pub session_row_id: Option<i64>,
    pub host: String,
    pub project: String,
    pub session_id: Option<String>,
    pub ai_profile: Option<String>,
    pub priority: i64,
    pub cursor_event_id: Option<i64>,
    pub high_watermark_event_id: Option<i64>,
    pub attempts: i64,
    pub replay_range_id: Option<i64>,
}

#[cfg(test)]
mod progress_tests;
#[cfg(test)]
mod retry_regression_tests;
#[cfg(test)]
mod tests;
