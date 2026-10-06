# Pricing Config Product Spec

Status: Current contract
Date: 2026-10-06

## Problem

USD cost estimates use compiled per-family rates plus `REMEM_PRICE_*`
environment variables. Operators cannot review or persist a price override in
the same `config.toml` that already holds Memory AI and SessionStart budgets.

## Goals

- Global USD overrides have a `config.toml` home under `[pricing]`.
- Per-family overlays live under `[pricing.<family>]` using the existing
  family names (`opus`, `sonnet`, `haiku`, `gpt55`, `gpt54`, `gpt54_mini`,
  `gpt54_nano`, `gpt5_codex`, `gpt5`, `gpt4`, `codex_mini`).
- `REMEM_PRICE_*` remains an env escape hatch with the previous parse rules.
- Present-but-invalid `[pricing]` numbers fail closed.
- Missing file or empty `[pricing]` keeps today's compiled family table.

## Non-Goals

- Changing compiled family rates, including GPT-5.6 credit models staying
  unpriced unless a global override is set.
- Writing compiled family rates into `config.toml` on init (that would pin
  stale prices across remem upgrades).
- Dropping `REMEM_PRICE_*`.

## Behavior

Precedence:

1. Global env (`REMEM_PRICE_INPUT_PER_MTOK` and
   `REMEM_PRICE_OUTPUT_PER_MTOK` both set)
2. Global `[pricing]` (`input_per_mtok` and `output_per_mtok` both set)
3. Family env field overlays
4. Family `[pricing.<family>]` field overlays
5. Compiled family table

A global override still applies to GPT-5.6 credit models. Family tables do
not. Set a global override with
`remem config set pricing.input_per_mtok 1.25` and
`remem config set pricing.output_per_mtok 6.5`.

### Usage evidence and cost coverage (GH-1102, GH-1105)

Each dispatched Memory AI attempt has an independent outcome and usage
record. A failed process, missing final-output file, or empty response does
not erase usage already received from that attempt. The original failure
still reaches the caller; a retry is another attempt, not a duplicate to
deduplicate. A failed attempt with no counters records missing usage and
does not estimate paid tokens from prompt length.
Cancellation by an outer timeout also preserves counters already received.

Provider/log usage distinguishes complete, partial, missing, and invalid
counters. An explicitly reported zero remains an observation. Missing or
null counters remain unavailable; strings, negative counts, inconsistent
subtotals, and arithmetic overflow are invalid. Valid counters from an
incomplete event remain available, including raw totals and cache/reasoning
breakdowns. The successful Claude text-only adapter retains its separately
labeled prompt/output length estimate.
Contradictory subtotals remain available for diagnosis but cannot inflate a
valid reported gross token total.

Cost coverage is independent from counter completeness. For example, Codex
may report raw output without a reasoning split. Equal output/reasoning
rates can price that raw total; different configured rates cannot. Only
the priceable portion contributes to the existing USD field. Unknown or
invalid pricing preserves the usage row and reports unavailable pricing.
The automatic Codex model placeholder is unpriced unless an operator sets
an explicit global override; it does not identify the model Codex selected.

Usage summaries, daily/weekly and source groups, API stats, latest-session
status, and timeline reports present the known priced portion alongside
coverage counts. They must not present an unpriced or partly priced attempt
as a complete zero-dollar estimate. These are local estimates, not provider
invoices. Historical rows retain their counters and costs with unverified
completeness; old provenance labels cannot reconstruct absent evidence.

## Done when

- Fresh config text contains an empty `[pricing]` table and no default rates.
- A complete `[pricing]` pair overrides every model, including GPT-5.6.
- A valid env pair still wins over `[pricing]`.
- A present non-numeric, negative, or one-sided global `[pricing]` fails
  closed.
- Family `[pricing.haiku]` overlays only that family after env is unset.
- Doctor fails when `[pricing]` is invalid.
- A synthetic failed attempt reporting 140 tokens followed by a successful
  140-token attempt records two attempts and 280 reported tokens.
- Zero, missing, partial, invalid, and text-estimated usage remain distinct.
- An unknown-price event, including one mixed with known cost, makes every
  aggregate's incomplete coverage visible without changing compiled rates.
