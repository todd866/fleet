# AI usage history

`fleet usage import FILE` validates and retains a metadata-only daily source snapshot.
`fleet usage status` reads the latest summary. `fleet usage refresh` regenerates local
exports without pretending that the upstream source was checked again.

State lives under `~/.local/state/fleet/ai-usage/`: SQLite daily observations keyed by
source/date, content-addressed immutable normalized imports, `daily.json` and
`summary.json`. Retention is indefinite: deleting a transcript must not delete its
usage history. No raw prompts, credentials, cookies, account identifiers or chat text
are retained. Duplicate imports are idempotent; old captures cannot overwrite newer
ones. A narrower recent export preserves prior dates.

Only the configured publisher writes the summary into the existing mac-sync share.
Set `publish_shared: true` in its `~/.config/fleet/ai-usage.json`; readers leave it false.
An empty local database never replaces shared history. The existing
morning digest reads that summary; no daemon or scheduler is installed. It raises a
refresh-due work item after 72 hours since the source was captured, or when its last
reported day is over five days old. Regenerating summary does not reset those clocks.
Usage history and remaining quota are distinct rows.

## Import contract

```json
{"source":"codex-account","captured_at":1794900000,
 "missing_rows":0,"daily":[{"date":"2026-11-15","total":123}]}
```

Sources: `codex-account`, `cursor-account`, `codex-local`, `claude-local`,
`deepseek-local`, `custom-local`. Optional per-day categories: `uncached_input`,
`output`, `cache_read`, `cache_write`, `unclassified_total_difference`.
Missing category values remain null, never inferred zero. Missing dates remain absent.
Counts are integers >=0; source/date is unique. Unknown fields are dropped.

Official Codex profile totals replace local totals for account reporting; local category
counters are an incomplete overlapping subset and must never be added to official
usage. Cursor account exports replace CLI bridge records. Claude-format transcripts
must be globally deduplicated by message/request ID across backups, selecting the
maximum four top-level usage counters rather than summing streaming fragments or nested
iteration summaries. Attribute models separately for DeepSeek and other backends.

## Current limits

This is durable retention, reporting and stale-data detection, **not an authenticated
account scraper**. Account refresh still requires an authorized browser export/import.
There is no stored login or background credential. The collector does not scan chat
files or silently open a browser in the morning. Automated provider adapters and
incremental transcript ingestion are follow-up work; keep stale warnings until a real
new source capture is imported. Do not label this live account reporting.

Tests: `python3 -m pytest tests/test_ai_usage.py tests/test_morning*.py`.

Custom source display labels are site config, not code: `~/.config/fleet/ai-usage.json`,
for example `{"labels":{"custom-local":"Local model"}}`.

This initial ledger has one imported account per named source. Additional same-provider
accounts require distinct configured source identities before import; do not overwrite
one account with another. Automated adapters must retain that distinction.
