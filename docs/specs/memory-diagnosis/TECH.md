# Memory diagnosis implementation

Status: Current contract
Date: 2026-10-07

## Decision

Adapt Remem's existing diagnostic and evidence boundaries. This is a doctor
composition, not a new collector, status registry, retrieval engine or service.
The requested technology and scope are already fixed. Reuse `search_raw_messages`,
`query_raw_session_messages`, `classify_memory`, active suppressions and
`project_current_truth`; join existing captured_events, extraction_tasks,
observations, session_summaries, memory_candidates, user_context_candidates,
memories and context_injection_items using their original identities.

Keep SQL read-only behind `open_db_read_only_current` and one read transaction.
Serialize a command-specific schema_version=1 report with five stages; bound
individual result lists at 20 with has_more. Human output groups retained
source/status counts and shows three samples per source; JSON keeps the full
retained report. Stored text matching and explicit
source linkage are reported as discovery evidence, not semantic proof.

For context, select the latest recorded run for project and optional host, or
validate an explicit run ID against that scope. Match canonical memory/user
claim IDs only inside that run. Do not substitute a different historical run
when a selected run has no matching item. No audit means unknown, including
retention expiry and preselection outside the audit's coverage.

## Measured archive query fix

Real local diagnosis exposed a project-first join plan in the bundled SQLite
3.45.3: the existing raw search repeated MATCH for each project row and timed
out after 30 seconds. Keep the existing FTS query first with CROSS JOIN, then
apply the unchanged project/branch/role/time filters and descending pagination.
The identical encrypted snapshot and query returned the same raw ID in 0.028
seconds with this plan; all five sampled phrases took 0.023–0.038 seconds in
the library probe. Verify the CLI separately; this probe is not end-to-end
acceptance. No index, schema, result contract or error fallback is added.

## Verification

Focused migrated-database tests and CLI selector tests, then repository format,
check, local-onnx suite and full PR preflight. Version manifests and CLI surface
contract must include the new command before submission. Real local evidence
receipts distinguish completed diagnostics from fresh synthetic fixtures.


## Real local acceptance (2026-10-07)

Clean source `4d9329b9` (0.6.104/schema96) completed ten diagnostics: five
actual user-message excerpts and five exact raw-session tuples. All exited 0.
Phrase diagnostics took 6.259–11.770 seconds; exact sessions 0.172–0.637 seconds.
A consistent encrypted backup of the real schema 91 database was migrated only
in isolation. Before/after diagnostic database hashes matched. Initial stale
schema failure and pre-fix query timeouts remain separate failed receipts.
Private user text, source session IDs, backups and detailed outputs are excluded
from Git. The five phrases are excerpts capped at 60 characters, not a retrieval
quality benchmark or human acceptance test.

The reports show raw provenance, capture ledger, extraction tasks, session
summaries, pending-review candidates and recorded context drops. Across five
session reports, 34 retained tasks were done, 54 candidates pending_review and five
matching summary context items dropped. These are displayed-row counts, not
unique global totals. No linked current memory was found, so all real validity
stages are unknown; fixture coverage for expiry/suppression/CurrentTruth must
not be described as actual local-user coverage. Source linkage and task
completion do not prove sentence-level preservation or live-model recall.

The static CLI contract contains 199 current entries, including one new doctor
command; the inventory does not imply 199 commands were run. Acceptance used ten
`doctor memory` invocations, one successful `raw sessions` listing and five
`raw messages` provenance reads. Fixture regressions, automated real data
checks, human usability and live-model answer quality remain distinct.

## Final production-source repeat (2026-10-08)

Clean source `7ed524cc`, production input tree
`64aa00312cb034ab7c1a7debd8f6328e5c515303e55b6e4137ca514ddc0b907a`,
passed format, default check and build, then all ten real-data JSON diagnostics
on the same isolated encrypted backup. Phrase queries took 5.620–6.463 seconds;
exact sessions 0.146–0.424 seconds. The database hash remained unchanged.

One additional terminal invocation returned all five stages in 7,603 bytes,
with retained source/status summaries, three samples per source and a JSON
detail hint (the previous terminal output was 32,498 bytes). This is automated
formatting verification, not human usability acceptance. The initial repeat
script put `--json` before `doctor`; those ten argument-error exits remain
failed receipts, followed by the corrected command-specific calls above.

An isolated source install also completed install/dry-run, status/context,
plugin activation and npm local installation/version checks. Eleven of twelve
commands exited 0; doctor exited 1 with only the expected fresh-store absence
of a capture heartbeat. The dry run created no files. Plugin/npm checks used
the explicit local binary; this is not published-channel or live-host evidence.
Human fault-location usability and live-model recall quality remain unperformed.
