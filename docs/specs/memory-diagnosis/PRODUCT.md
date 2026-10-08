# Memory diagnosis

Status: Current contract
Date: 2026-10-07

## User outcome

`remem doctor memory "original phrase"` or an exact raw-session selector shows
one read-only report: capture → extraction → review → current validity → context.
It answers why a remembered fact may be unavailable using existing evidence.

## Contract

- `--project` is an exact stored project key; `--cwd` derives the usual key.
- A session uses the unchanged `host`, `source_root`, `project`, `session_id`
  tuple from `raw sessions`. Host collisions never merge.
- Query discovery uses existing raw search plus literal matches in stored
  capture/candidate/memory text. No match is unknown, never proof of no capture.
- Each surface retains at most 20 rows and reports omitted results. Terminal
  output summarizes retained rows/statuses and shows three samples per source;
  `--json` exposes all retained evidence.
- Extraction task state/cursors, observations and rollup coverage are distinct
  evidence; a completed task does not prove a particular phrase was distilled.
- Review shows recorded status, block/review reasons and source references,
  including ordinary memory and user-context candidates.
- Validity reuses current memory classification, suppression and CurrentTruth;
  it is evaluated now and does not invent historical eligibility.
- Context reports one explicitly requested injection run or the latest recorded
  run in the selected project/host. Its ID and time are visible. Only matching
  per-item audits prove injection or a drop; absence means unknown. A source
  session is not assumed to be the destination session.
- Missing database, schema errors, unresolved exact raw-session provenance,
  and failed reads remain errors. Unknown evidence is a successful diagnostic,
  not an assertion that capture or recall succeeded. No DB, logs or config writes.
- Output redacts sensitive text through the existing redaction boundary.

## Ownership

Remem helps an Agent continue work and owns capture, governed memory and recall.
Refine helps people retrieve, inspect and reuse knowledge through Remem's raw
session references and existing commit/session APIs. agent-sessions parses
native source formats. Keep one capture/archive and reuse source identifiers;
no downstream transcript store, new speculative schema or parked #934/#935 work.

## Acceptance

Prove pending/rejected candidates, failed extraction, expired/suppressed memory,
recorded context drops, missing audits and same-ID cross-host separation on real
migrated SQLite fixtures. Separately run read-only diagnosis on real local
questions/sessions and record commands, surfaces and limits; fixture results do
not establish human usability or live model quality.
