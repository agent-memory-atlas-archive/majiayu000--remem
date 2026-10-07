# Candidate Auto-Promotion Technical Contract

Status: Current contract

Date: 2026-08-03; evidence reassessment amendment: 2026-10-06

Tracking: #955, #1105

## Content identity and evidence reassessment (GH-1105)

Ordinary candidate deduplication distinguishes exact source replay from a new
evidence snapshot. Under the candidate persistence transaction, it loads all
matching content identities. Only automatically created observation/summary
`pending_review` rows with no review, quarantine, acknowledgement, active-memory,
or activation binding can be replaced by new evidence. Previously system-replaced
rows participate in the seen-evidence set; they never erase a later human terminal
decision. Event identities are compared as positive integer sets, so ordering
does not affect idempotency. The incoming set must introduce a trusted event,
remain at least as trusted and confident as the pending snapshot, and pass the
same source, routing, risk, and poisoning gates as every new candidate.

The writer uses an immediate transaction before identity and suppression reads.
It writes a fresh candidate, then marks prior pending versions `discarded` with
`review_action_source='candidate_evidence_superseded'`, an exact replacement id
and evidence digest, and a version-checked update. Original evidence/payloads and
activation receipts are never rewritten. A failed promotion, audit, or version
check rolls the entire replacement back. Topic/pattern/entity suppression and
matching suppressed memory identities veto reassessment. Preference reinforcement
keeps its current evidence-aware path; native/Dream/pack identities are ineligible.

Operational-state TTL renewal remains a separate existing lifecycle path. An
untouched `auto_promoted` observation/summary row whose TTL has elapsed may be
renewed; a legacy unattributed automatic row missing TTL retains its existing
upgrade path. Such rows are not rewritten or system-discarded. Their old event
identities still count as seen, so renewal needs a new trusted event and at
least the old confidence/trust. Their completed activation does not veto this
fresh renewal, while any human terminal row, quarantine, or applicable
suppression does. The ordinary lifecycle planner records memory replacement.

## Risk Rubric

`memory_candidate/prompt.rs` owns the extraction prompt and defines the closed
rubric:

- `low`: already-true repository-local claims directly supported by supplied
  evidence, including an observed failure lesson;
- `medium`: preferences, procedures, recommendations, inferences, proposals,
  future plans, or applicability that needs review;
- `high`: credentials, auth/authz state, private/personal/payment data,
  destructive operations, or other security-sensitive claims.

`parse::normalize_risk_class` accepts only those three values. The extraction
report contains explicit low/medium/high counts, whose sum must equal all
candidate predictions.

## Claim-Level Support

`support::has_claim_level_source_support` normalizes whitespace/case, splits a
candidate into sentence claims, and requires every claim to match at least one
eligible source observation. A source can support one claim and another source
can support the next; one supported sentence cannot mask an unsupported one.

Exact and ordered-overlap matching retain the existing actor, identifier,
security-modifier, ordering, and minimum-overlap checks. Semantic signatures
add explicit handling for negative polarity, uncertainty, prospective language,
prescriptive language, and conditions.

Candidate/source signatures must match. Only an empty signature or negative
factual signature is auto-promotable. `won't` and `can't`, including curly
apostrophes, normalize to their real prospective/capability modal forms.
Outer meta-negation is a separate signature so a false or incorrect quoted
claim cannot support its embedded text. This permits supported negative facts
but keeps future, uncertain, conditional, prescriptive, modal, and ambiguous
quoted claims pending.

The structured claim gate also review-routes imperative control text,
auth/authz control state, and affirmative destructive actions even if the
model labels them `low`. A locally negated destructive fact such as “does not
delete active rows” remains eligible; the gate does not return to a whole-text
common-word blacklist.

## Type and Secret Gates

The canonical `MemoryType::auto_promote` vocabulary remains unchanged for core
types. The observation-path decision adds a narrow failure-lesson case:

- type is `lesson`;
- candidate and an eligible bugfix/decision observation both contain an
  affirmative failure outcome linked to a recovery action in the same claim;
- ordinary scope, risk, confidence, trust, routing, evidence-id, unsafe-marker,
  and claim-support gates all pass.

Non-failure lessons record `lesson_not_failure_qualified`; preference and
procedure candidates record `memory_type_not_auto_promotable`.

The unsafe-marker list removes bare `token`. A deterministic canonicalizer
folds case and full-width ASCII and treats punctuation, underscores, dashes,
and Unicode separators as word boundaries. Credential-qualified forms such as
`access-token`, `GITHUB_TOKEN`, `deployment token`, and `OAuth2 token` are
blocked. Only explicit non-credential contexts such as token budgets, counts,
limits, and windows bypass this token check; ambiguous contexts fail closed.
Existing API-key, bearer, password, private-key, payment, secret, and key-format
markers remain. Instruction-pattern scanning and quarantine run before the
promotion decision, with the claim gate as a second defense for generic
imperatives outside the fixed scanner vocabulary.

## Verification

- `cargo test --lib memory_candidate::tests::reassessment`
- `cargo test --lib memory_candidate::tests::ttl`
- `cargo test memory_candidate::tests_autopromote`
- `cargo test memory_candidate::tests_autopromote_gh955`
- `cargo test eval::extraction`
- `cargo run -- eval-extraction --json --check-baseline`
- `cargo fmt --check`
- `cargo check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
