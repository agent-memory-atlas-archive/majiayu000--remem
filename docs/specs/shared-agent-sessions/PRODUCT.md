# Shared agent session parsing

Status: Current contract (implementation in progress)
Date: 2026-09-25

## Goal

Use agent-sessions 0.2 for Claude Code / Codex JSONL framing, native message and tool projection, and file discovery while preserving Remem's lossless archive and memory-quality policies.

## Acceptance

1. Raw physical rows, occurrence identity, empty messages, metadata/control messages, timestamp precedence and user/assistant roles retain existing meaning.
2. Captured boundaries reject short files before delivering the pending final row; appends beyond the boundary are ignored. Invalid UTF-8 remains an error. Only one pending row is retained.
3. Default scanning honors CLAUDE_CONFIG_DIR and CODEX_HOME. Empty overrides return an error. Nonempty overrides are required sources: missing, non-directory or unreadable projects/sessions directories fail the scan; only inferred default directories may be absent. Explicit HOST:LABEL=PATH roots remain supported; missing required roots fail and subagent descendants remain excluded.
4. Malformed candidate Codex tool rows, string-only tool arguments/output, success markers and resolved Git SHA checks keep their current contract.
5. Source classification uses shared precedence: subagent evidence wins, native exec stays unattended even when the originator says Codex Desktop, and IDE is interactive. Unattended is an existing mode label, not proof that no person initiated the session.
6. Existing saved classifications upgrade once from the legacy policy to the shared policy only when native evidence confirms the legacy stored mode. An additive classifier-version column distinguishes legacy rows from new-policy rows; it does not change raw identity, archived rows, capture activation, extraction or memory policy. Missing mode evidence defers upgrading a known legacy mode. Host conflicts and actual known-mode conflicts remain errors, and failed batches cannot partially upgrade classifications.
7. Isolated regression tests, full runtime suite, formatting/check/clippy, release metadata and first-run smoke pass before delivery.

## Selected Codex profile consistency (GH-1104)

Native installation, automatic host detection, dry-run, uninstall, doctor,
transcript scanning and default native-memory import use the same selected
Codex root: `CODEX_HOME` when set, otherwise the inferred home `.codex`.
An explicit root must be nonempty and absolute. Invalid configuration fails
before install/uninstall writes and is reported by doctor; it never falls
back to another profile. A valid absolute root may be created by explicit
installation. Scanning still requires its selected `sessions` source to
exist. Other hosts and explicitly supplied import source paths retain their
existing selection rules. Installation owns only remem entries and preserves
unrelated Codex configuration and hooks.

## Distribution

Use the registry dependency. Local validation may use an external Cargo patch while agent-sessions is unpublished. Publication and registry lockfile verification are separate delivery gates. The specification records user-approved ecosystem work; issue/spec/implementation PR links are prepared by the integration owner before remote submission.
