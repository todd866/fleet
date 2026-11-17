"""Durable, metadata-only AI usage history. No credentials, scraping or network I/O."""
import argparse
import hashlib
import json
import os
import sqlite3
import time
import tempfile
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

CATEGORIES = ('uncached_input', 'output', 'cache_read', 'cache_write', 'unclassified_total_difference')
SOURCES = {'codex-account': ('Codex', 'account'), 'cursor-account': ('Cursor', 'account'),
           'claude-local': ('Claude', 'local'), 'deepseek-local': ('DeepSeek', 'local'),
           'custom-local': ('Other models', 'local'), 'codex-local': ('Codex', 'local')}


def state_dir(home):
    return Path(home) / '.local/state/fleet/ai-usage'


def atomic(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, prefix=path.name + '.', delete=False) as handle:
        handle.write(json.dumps(data, indent=2) + '\n')
        tmp = Path(handle.name)
    try:
        os.replace(tmp, path)
    finally:
        tmp.unlink(missing_ok=True)


def validate(document):
    """Strict allowlist: prompts, account identifiers and credentials never retained."""
    source = document['source']
    if source not in SOURCES:
        raise ValueError('unsupported source')
    captured = float(document['captured_at'])
    if not 0 < captured <= time.time() + 300:
        raise ValueError('invalid capture time')
    missing = document.get('missing_rows', 0)
    if type(missing) is not int or missing < 0:
        raise ValueError('invalid missing row count')
    rows = []
    seen = set()
    for item in document['daily']:
        day = date.fromisoformat(item['date']).isoformat()
        if day in seen:
            raise ValueError('duplicate date')
        seen.add(day)
        row = {'date': day}
        for key in ('total', *CATEGORIES):
            value = item.get(key)
            if value is not None and (type(value) is not int or value < 0):
                raise ValueError('invalid token counter')
            row[key] = value
        rows.append(row)
    if not rows:
        raise ValueError('empty daily series')
    return {'schema': 1, 'source': source, 'captured_at': captured,
            'missing_rows': missing, 'daily': sorted(rows, key=lambda r: r['date'])}


def connect(home):
    root = state_dir(home)
    root.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(root / 'history.sqlite3')
    db.execute('CREATE TABLE IF NOT EXISTS sources (source TEXT PRIMARY KEY, captured REAL, first_day TEXT, last_day TEXT, missing_rows INTEGER)')
    db.execute('CREATE TABLE IF NOT EXISTS daily (source TEXT, day TEXT, payload TEXT, PRIMARY KEY(source,day))')
    return db


def ingest(home, document):
    doc = validate(document)
    root = state_dir(home)
    digest = hashlib.sha256(json.dumps(doc, sort_keys=True).encode()).hexdigest()
    # Content-addressed evidence is retained indefinitely, outside source control.
    archive = root / 'imports' / (digest + '.json')
    if not archive.exists():
        atomic(archive, doc)
    with connect(home) as db:
        previous = db.execute('SELECT captured FROM sources WHERE source=?', (doc['source'],)).fetchone()
        if previous and previous[0] > doc['captured_at']:
            raise ValueError('older source capture cannot replace newer data')
        for row in doc['daily']:
            db.execute('INSERT OR REPLACE INTO daily VALUES (?,?,?)', (doc['source'], row['date'], json.dumps(row)))
        span = db.execute('SELECT min(day),max(day) FROM daily WHERE source=?', (doc['source'],)).fetchone()
        db.execute('INSERT OR REPLACE INTO sources VALUES (?,?,?,?,?)', (doc['source'], doc['captured_at'], *span, doc['missing_rows']))
    return refresh(home)


def refresh(home, now=None):
    now = time.time() if now is None else now
    with connect(home) as db:
        sources = []
        for source, captured, first, last, missing in db.execute('SELECT * FROM sources ORDER BY source'):
            provider, scope = SOURCES[source]
            try:
                labels = json.loads((Path(home) / '.config/fleet/ai-usage.json').read_text()).get('labels', {})
                label = labels.get(source)
                if isinstance(label, str) and 0 < len(label) <= 40 and label.isprintable():
                    provider = label
            except (OSError, ValueError, AttributeError):
                pass
            sources.append({'source': source, 'provider': provider, 'scope': scope, 'captured_at': captured,
                            'coverage_start': first, 'coverage_end': last, 'missing_rows': missing,
                            'refresh': 'fleet usage import <normalized-metadata.json>'})
        daily = [{'source': s, **json.loads(p)} for s, p in db.execute('SELECT source,payload FROM daily ORDER BY day,source')]
    today = datetime.fromtimestamp(now, timezone.utc).date()
    week = today - timedelta(days=today.weekday() + 7)
    end = week + timedelta(days=6)
    for source in sources:
        selected = [r for r in daily if r['source'] == source['source'] and str(week) <= r['date'] <= str(end)]
        totals = [r['total'] for r in selected if r['total'] is not None]
        source['last_full_week'] = {'start': str(week), 'end': str(end), 'observed_days': len(selected),
            'total': sum(totals) if totals else None,
            'scope': source['scope'], 'coverage': 'observed only; missing dates unknown'}
    summary = {'schema': 1, 'generated_at': now, 'sources': sources,
               'note': 'Sources are overlapping observations, never additive across account and local scope. Missing dates/categories are unknown.'}
    atomic(state_dir(home) / 'summary.json', summary)
    atomic(state_dir(home) / 'daily.json', {'schema': 1, 'daily': daily})
    shared = Path(home) / '.local/state/mac-sync'
    try:
        publish = json.loads((Path(home) / '.config/fleet/ai-usage.json').read_text()).get('publish_shared') is True
    except (OSError, ValueError, AttributeError):
        publish = False
    if shared.is_dir() and sources and publish:
        atomic(shared / 'fleet-ai-usage/summary.json', summary)
    return summary


def read_summary(home):
    # Shared copy lets the existing hub morning digest see primary-host imports.
    paths = [Path(home) / '.local/state/mac-sync/fleet-ai-usage/summary.json', state_dir(home) / 'summary.json']
    valid = []
    for p in paths:
        try:
            doc = json.loads(p.read_text())
            if isinstance(doc, dict) and doc.get('schema') == 1 and isinstance(doc.get('sources'), list) and doc['sources']:
                valid.append(doc)
        except (OSError, ValueError, TypeError):
            pass
    return max(valid, key=lambda x: x.get('generated_at', 0)) if valid else None


def main(argv=None):
    parser = argparse.ArgumentParser(prog='fleet usage')
    parser.add_argument('--home', default=str(Path.home()))
    sub = parser.add_subparsers(dest='action', required=True)
    imp = sub.add_parser('import'); imp.add_argument('file')
    sub.add_parser('status'); sub.add_parser('refresh')
    args = parser.parse_args(argv)
    if args.action == 'import':
        result = ingest(args.home, json.loads(Path(args.file).read_text()))
    elif args.action == 'refresh':
        result = refresh(args.home)
    else:
        result = read_summary(args.home) or {'sources': [], 'state': 'not configured'}
    print(json.dumps(result, indent=2))
    return 0
