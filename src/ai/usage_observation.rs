use serde::Serialize;
use serde_json::Value;

use super::types::TokenUsage;

pub(super) const FIELDS: [&str; 7] = [
    "input_tokens",
    "output_tokens",
    "reasoning_tokens",
    "cache_creation_tokens",
    "cache_read_tokens",
    "raw_input_tokens",
    "raw_output_tokens",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageStatus {
    Complete,
    Partial,
    Missing,
    Invalid,
    Estimated,
}

impl UsageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::Estimated => "estimated",
        }
    }
}

/// Numeric fields contain known portions only. Missing/invalid fields remain
/// explicit alongside those values; zero alone never asserts an observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct UsageObservation {
    pub tokens: TokenUsage,
    pub known_total_tokens: i64,
    pub status: UsageStatus,
    pub missing_fields: Vec<&'static str>,
    pub invalid_fields: Vec<&'static str>,
    pub has_reported_counters: bool,
    /// Keep turns separate when missing categories differ between turns.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<UsageObservation>,
}

impl UsageObservation {
    pub fn missing() -> Self {
        Self {
            tokens: TokenUsage::default(),
            known_total_tokens: 0,
            status: UsageStatus::Missing,
            missing_fields: FIELDS.to_vec(),
            invalid_fields: Vec::new(),
            has_reported_counters: false,
            parts: Vec::new(),
        }
    }

    pub fn estimated(input: i64, output: i64) -> Self {
        Self {
            tokens: TokenUsage::estimated(input, output),
            known_total_tokens: input.checked_add(output).unwrap_or(0),
            status: UsageStatus::Estimated,
            missing_fields: Vec::new(),
            invalid_fields: Vec::new(),
            has_reported_counters: false,
            parts: Vec::new(),
        }
    }

    pub(super) fn from_counters(counters: [Counter; 7], has_reported_counters: bool) -> Self {
        let mut observation = Self {
            tokens: TokenUsage {
                input_tokens: counters[0].value(),
                output_tokens: counters[1].value(),
                reasoning_tokens: counters[2].value(),
                cache_creation_tokens: counters[3].value(),
                cache_read_tokens: counters[4].value(),
                raw_input_tokens: counters[5].value(),
                raw_output_tokens: counters[6].value(),
            },
            status: UsageStatus::Complete,
            known_total_tokens: 0,
            missing_fields: Vec::new(),
            invalid_fields: Vec::new(),
            has_reported_counters,
            parts: Vec::new(),
        };
        for (field, counter) in FIELDS.into_iter().zip(counters) {
            match counter {
                Counter::Missing => observation.missing_fields.push(field),
                Counter::Invalid => observation.invalid_fields.push(field),
                Counter::Known(_) => {}
            }
        }
        let input = Counter::known_gross(counters[5], &[counters[0], counters[3], counters[4]]);
        let output = Counter::known_gross(counters[6], &[counters[1], counters[2]]);
        if let Some(total) =
            input.and_then(|input| output.and_then(|output| input.checked_add(output)))
        {
            observation.known_total_tokens = total;
        } else {
            observation.invalid_fields.push("total_tokens");
        }
        observation.refresh_status();
        observation
    }

    pub fn field_known(&self, field: &str) -> bool {
        self.status != UsageStatus::Missing
            && !self.missing_fields.contains(&field)
            && !self.invalid_fields.contains(&field)
    }

    pub(super) fn merge(&mut self, other: Self) {
        if self.parts.is_empty() {
            self.parts.push(self.clone());
        }
        if other.parts.is_empty() {
            self.parts.push(other.clone());
        } else {
            self.parts.extend(other.parts.clone());
        }
        if let Some(total) = self
            .known_total_tokens
            .checked_add(other.known_total_tokens)
        {
            self.known_total_tokens = total;
        } else {
            self.invalid_fields.push("total_tokens");
        }
        macro_rules! add_field {
            ($field:ident) => {
                if let Some(sum) = self.tokens.$field.checked_add(other.tokens.$field) {
                    self.tokens.$field = sum;
                } else {
                    self.invalid_fields.push(stringify!($field));
                }
            };
        }
        add_field!(input_tokens);
        add_field!(output_tokens);
        add_field!(reasoning_tokens);
        add_field!(cache_creation_tokens);
        add_field!(cache_read_tokens);
        add_field!(raw_input_tokens);
        add_field!(raw_output_tokens);
        self.has_reported_counters |= other.has_reported_counters;
        self.missing_fields.extend(other.missing_fields);
        self.invalid_fields.extend(other.invalid_fields);
        self.refresh_status();
    }

    fn refresh_status(&mut self) {
        self.missing_fields.sort_unstable();
        self.missing_fields.dedup();
        self.invalid_fields.sort_unstable();
        self.invalid_fields.dedup();
        self.status = if !self.invalid_fields.is_empty() {
            UsageStatus::Invalid
        } else if !self.has_reported_counters {
            UsageStatus::Missing
        } else if !self.missing_fields.is_empty() {
            UsageStatus::Partial
        } else {
            UsageStatus::Complete
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Counter {
    Known(i64),
    Missing,
    Invalid,
}

impl Counter {
    fn known_gross(raw: Self, categories: &[Self]) -> Option<i64> {
        match raw {
            // Retain contradictory subtotals in details, but never inflate a
            // valid reported gross input/output total with those subtotals.
            Self::Known(total) => Some(total),
            Self::Invalid => Some(0),
            Self::Missing => categories.iter().try_fold(0_i64, |sum, category| {
                if let Self::Known(value) = category {
                    sum.checked_add(*value)
                } else {
                    Some(sum)
                }
            }),
        }
    }
    pub fn read(value: &Value, key: &str) -> Self {
        match value.get(key) {
            None | Some(Value::Null) => Self::Missing,
            Some(value) => match value.as_i64() {
                Some(value) if value >= 0 => Self::Known(value),
                _ => Self::Invalid,
            },
        }
    }

    pub fn optional_zero(value: &Value, key: &str) -> Self {
        if value.get(key).is_none() {
            Self::Known(0)
        } else {
            Self::read(value, key)
        }
    }

    pub fn value(self) -> i64 {
        if let Self::Known(value) = self {
            value
        } else {
            0
        }
    }
    pub fn is_reported(self) -> bool {
        matches!(self, Self::Known(_))
    }

    pub fn subtract(self, subset: Self) -> Self {
        match (self, subset) {
            (Self::Known(total), Self::Known(part)) if part <= total => Self::Known(total - part),
            (Self::Known(_), Self::Known(_)) | (Self::Invalid, _) | (_, Self::Invalid) => {
                Self::Invalid
            }
            _ => Self::Missing,
        }
    }

    pub fn sum(values: &[Self]) -> Self {
        let mut total: i64 = 0;
        let mut missing = false;
        for value in values {
            match value {
                Self::Known(value) => match total.checked_add(*value) {
                    Some(sum) => total = sum,
                    None => return Self::Invalid,
                },
                Self::Missing => missing = true,
                Self::Invalid => return Self::Invalid,
            }
        }
        if missing {
            Self::Missing
        } else {
            Self::Known(total)
        }
    }
}
