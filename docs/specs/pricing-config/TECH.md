# Pricing Config Technical Spec

Status: Current contract
Date: 2026-10-06

## Shape

```toml
[pricing]
# optional global override; both input and output are required together
# input_per_mtok = 1.25
# output_per_mtok = 6.5
# reasoning_per_mtok = 6.5
# cache_creation_per_mtok = 1.25
# cache_read_per_mtok = 1.25

# [pricing.haiku]
# input_per_mtok = 1.0
# output_per_mtok = 5.0
```

Accepted global and family keys: `input_per_mtok`, `output_per_mtok`,
`reasoning_per_mtok`, `cache_creation_per_mtok`, `cache_read_per_mtok`.
Integers and finite floats `>= 0` are accepted. Unknown keys or family
tables fail closed.

## Resolution

`runtime_config::global_pricing_override()` reads `REMEM_CONFIG` / the
data-dir `config.toml` and does not write. `ensure_config_defaults` inserts
an empty `[pricing]` table only on init/show/set; it does not write rates.

`ai::pricing` applies env first, then that global override, then family env,
then family `[pricing.<family>]`, then the compiled table.

A global `[pricing]` table with neither input nor output and no other known
keys is a no-op. One of input/output without the other, or optional keys
without both input and output, is an error. An unreadable TOML file is an
error.

Invalid pricing config logs at error level, preserves observed usage, and
marks the event unpriced with `pricing_source = 'invalid_pricing'`. A zero
numeric placeholder is never a complete estimate. Doctor
`check_runtime_config` continues to call `validate_pricing_config()`.

## Observation and attempt boundary

`ai::usage_observation` owns the typed observation status and checked
counter arithmetic. Persisted evidence lists missing/invalid normalized
fields and retains every valid raw/input/output/cache/reasoning counter.
Null is unavailable; a present non-integer or negative value is invalid.
Whole-usage absence is missing, valid all-zero usage is preserved, and
complete means all supported billing categories can be resolved.

Anthropic input excludes its separately reported cache categories; omitted
optional cache fields follow that adapter's zero-default protocol, while
present null/invalid cache fields remain unavailable/invalid. Anthropic has
no separate reasoning billing category in this adapter. Codex raw input
includes cached input, and raw output includes reasoning. Missing Codex
cache/reasoning splits remain unknown: no clamping or fabricated observed
zero is permitted. Contradictory cache aliases or a subtotal exceeding its
raw total is invalid. Multiple `turn.completed` observations aggregate with
checked arithmetic and retain incomplete/invalid status across turns.
Known gross input/output totals prefer valid raw counters over contradictory
subtotals. When raw totals are missing, only safe known category portions
contribute; invalid raw totals do not manufacture a replacement gross count.

Codex parses available stdout usage before exit/final-output validation.
HTTP parses usage before response-text validation, including JSON error
responses when present. A typed backend failure carries that evidence to
`call_ai`, which records exactly once on both success and failure before
propagating the original error. Failures without available counters have
missing usage; only successful text-only Claude output uses a text estimate.
Process timeout or transport failure without received terminal telemetry
cannot supply unobserved provider counters.
A dispatch-scoped attempt guard survives cancellation of the backend future.
Codex updates its shared observation after each complete stdout event; guard
drop records a failed attempt with that evidence, or missing counters. Normal
success/error handling disarms the guard before recording, so completion and
drop do not create duplicate rows. Prompt writes, stdout/stderr drains, and
process wait all share the Codex deadline. Accounting remains a synchronous
best-effort database write with an explicit error log on persistence failure.

`codex-default` is an unknown-model placeholder. The global override paths
run first; absent such an override, it returns `unknown_pricing` before the
generic named Codex/GPT model-family lookup. Named model rates are unchanged.

## Persistence and aggregates

Migration v096 adds `usage_status`, `attempt_outcome`, `cost_status`, and
`usage_details_json` to `ai_usage_events`. Existing token/cost/source columns
remain. Old rows default to `legacy_unverified` status and `unknown` outcome;
no historical counter, cost, or source rewrite occurs. New statuses are
`complete`, `partial`, `missing`, `invalid`, or `estimated`; new outcomes
are `success` or `failed`. JSON details have a format version and bounded
field names, contain counter evidence only, and never copy prompt/output
text or arbitrary provider JSON.

`cost_status` is `complete`, `partial`, `unpriced`, or `legacy_unverified`.
The existing `estimated_cost_usd` is the known priced portion. An absent
category split can be priced from the raw total only when all possible
category rates are equal (or the raw remainder is zero). Otherwise only
known categories contribute and cost coverage is partial. Invalid usage
does not contribute a manufactured cost. Missing usage remains partial
cost coverage even when its zero placeholder could be multiplied by a
known rate. Invalid/non-finite cost calculations become unpriced errors.
Text-estimated usage retains its calculated amount but its cost status is
always partial. Per-turn pricing preserves that state when estimated and
observed parts are combined. Aggregates defensively count estimated,
missing, invalid, and legacy usage as incomplete even if an inconsistent
stored cost-status label claims complete coverage.

The shared `db::query::ai_usage` aggregate path exposes additive coverage
counts for complete/partial/missing/invalid/estimated/legacy usage, failed
attempts, unpriced calls, and incomplete cost. Each aggregate's cost and
coverage use identical time/project/session/group predicates. API fields
are additive; existing cost fields keep their known-portion semantics.
CLI output labels incomplete costs and does not classify every non-text
source as exact. `db::query::stats` delegates AI usage functions to the
dedicated module instead of growing its system-statistics implementation.

### Rust source compatibility

The staged source boundary adds `coverage: AiUsageCoverage` to `AiUsageTotals`,
`AiUsageSourceTotals`, `AiUsageBreakdown`, `DailyAiUsage`, and `WeeklyAiUsage`,
and `ai_usage_coverage` to `LatestSessionMemorySpend`. External struct literals
must provide these fields; exhaustive destructuring must bind them or use `..`.
Existing field reads and the query functions remain available. Callers should
use the coverage returned by those queries. A zero/default coverage value must
not stand in for nonempty historical usage: old rows remain
`legacy_unverified` with incomplete cost coverage.

REST additions preserve existing fields; clients that reject unknown fields
must accept the documented coverage fields. Missing coverage from an older
server is unknown, rather than complete zero cost. The public-surface manifest
stages the replacement Rust declaration fingerprints and records the exact
superseded published identities. This is a source compatibility change awaiting
release review, not a promotion of the published `v0.6.82` baseline.

Migration v096 is a forward database change. Older binaries reject a database
whose recorded schema is newer than they support. A rollback therefore uses a
compatible binary or the matching pre-upgrade backup; deleting migration markers
does not make the newer schema compatible.

## Verification

Use synthetic JSON, in-memory SQLite, and harmless local fake subprocesses.
Cover complete zeros, missing/null/string/negative/overflow counters,
Codex aliases and raw/subtotal consistency, independent failed/successful
attempt records, missing/empty output, legacy migration, unknown pricing
mixed with known cost, and different reasoning-rate overrides without a
reasoning split. No host CLI, model request, or private billing data is
needed. Compile-time schema/public-surface gates accompany the regression
tests; compiled family rates and pricing precedence remain unchanged.

## Files

- `src/runtime_config/pricing.rs`
- `src/ai/pricing.rs`
- `src/ai/usage.rs`
- `src/ai/usage_observation.rs`
- `src/ai/codex_usage.rs`
- `src/db/usage.rs`
- `src/db/query/ai_usage.rs`
- `src/doctor/runtime_config_check.rs`
