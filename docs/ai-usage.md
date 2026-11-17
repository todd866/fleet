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
files or silently open a browser in the morning. Automated provider account adapters remain follow-up work; keep account stale warnings
until a real new source capture is imported. The optional local transcript producer below
is separate from account export freshness. Do not label this live account reporting.

Tests: `python3 -m pytest tests/test_ai_usage.py tests/test_morning*.py`.

Custom source display labels are site config, not code: `~/.config/fleet/ai-usage.json`,
for example `{"labels":{"custom-local":"Local model"}}`.

This initial ledger has one imported account per named source. Additional same-provider
accounts require distinct configured source identities before import; do not overwrite
one account with another. Automated adapters must retain that distinction.


## Hourly local producer

Opt in on the source Mac with `local_collection.enabled: true` in the site config.
The existing hourly `fleet morning collect --local` command then runs a bounded
producer **before** collecting its ordinary read-only snapshot. No new launchd job.
Defaults: five seconds and64 MiB per pass, file-offset checkpoints, a nonblocking
single-writer lock. A first historical scan can be drained by repeated bounded calls
`fleet usage collect-local`; morning flags partial/error status until caught up.

Before enabling, use `fleet usage seed-local metadata.jsonl [metadata.jsonl.gz ...]`
to supply the deduplicated recovered baseline. Format per line:
`[message_id,request_id,UTC_timestamp,model,input,output,cache_write,cache_read]`.
Without a seed the collector refuses to overwrite existing aggregate history.
Metadata IDs are hashed in SQLite; prompts/content are never persisted. Repeated
fragments and copied transcripts update the same request only when its reported
counter total grows. Offsets resume partial lines; source deletion does not delete
usage. Files are read only under configured transcript roots.

`local_collection.model_sources` maps model prefixes to source keys; custom backend
names belong only in site config. This adapter currently handles Claude-format
transcripts on the opted-in host. It does not automatically collect other Macs or
Codex JSONL formats. Historical cross-Mac baseline remains, but live observations
must not be described as full account totals.

Optional `api_grants` in site config are distinct from subscription allowance and
usage. Fields: id, provider, expires_on, balance, currency, verified_at. Unknown
balance/provider stay null. A known expiry does not establish credit remaining.
The morning check warns within seven days of expiry; it does not spend credit.

Account refresh requires the existing authenticated browser: official Codex profile
response or Cursor date-range Export CSV. A scheduled assistant task may capture and
validate these without extracting credentials. No public personal-account API was
verified; never copy session cookies into a cron job. Failed/offline/login-required
captures must keep the previous captured_at unchanged.

The configured publisher also syncs daily aggregates and a consistent SQLite backup
`history.snapshot.sqlite3` via the existing mac-sync share when the database changes.
Never sync/open that backup as the active writable ledger; restore to private state first.
This protects retained request IDs across laptop loss without syncing a live SQLite file.


Browser refresh handoff commands (capture time must be the actual successful fetch,
not time an old file is reimported):

```
fleet usage import-cursor /path/usage.csv --captured-at 2026-11-17T08:00:00Z
fleet usage import-codex /path/profile.json --captured-at 2026-11-17T08:00:00Z
```

Codex normalized response contract: daily pairs `[UTC_date,tokens]`, lifetime_tokens,
coverage_start and coverage_end. Daily totals must reconcile to the official total.
The Cursor adapter checks its known CSV columns and counter reconciliation, retaining
blank-token row counts as unknown. It stores token metadata only, not cost, identifiers
or raw export content. Snapshot/date replacement avoids adding overlapping exports.
