"""Validate browser exports. No authentication, cookies or network access."""
import csv
import json
from collections import defaultdict
from datetime import datetime, date, timedelta, timezone
from pathlib import Path


def capture_epoch(value):
    stamp=datetime.fromisoformat(value.replace('Z','+00:00'))
    if stamp.tzinfo is None:raise ValueError('capture timestamp must include timezone')
    return stamp.timestamp()


def codex(path,captured):
    raw=json.loads(Path(path).read_text());pairs=raw['daily']
    daily=dict(pairs)
    if len(daily)!=len(pairs) or sum(daily.values())!=raw['lifetime_tokens']:
        raise ValueError('Codex daily buckets do not reconcile to official lifetime total')
    start=date.fromisoformat(raw['coverage_start']);end=date.fromisoformat(raw['coverage_end'])
    if start>end or (end-start).days>10000:raise ValueError('invalid coverage range')
    if any(not start<=date.fromisoformat(d)<=end for d in daily):raise ValueError('daily bucket outside coverage')
    rows=[];day=start
    while day<=end:
        rows.append({'date':str(day),'total':daily.get(str(day),0)});day+=timedelta(days=1)
    return {'source':'codex-account','captured_at':capture_epoch(captured),'daily':rows}


def cursor(path,captured):
    fields={'Input (w/ Cache Write)':'cache_write','Input (w/o Cache Write)':'uncached_input','Cache Read':'cache_read','Output Tokens':'output'}
    daily=defaultdict(lambda:defaultdict(int));missing=0
    with Path(path).open(newline='') as handle:
        reader=csv.DictReader(handle)
        if not set(fields)|{'Date','Total Tokens'} <= set(reader.fieldnames or []):
            raise ValueError('Cursor export columns changed')
        for row in reader:
            stamp=datetime.fromisoformat(row['Date'].replace('Z','+00:00'))
            if stamp.tzinfo is None:raise ValueError('Cursor export timestamp lacks timezone')
            day=stamp.astimezone(timezone.utc).date().isoformat()
            if not row['Total Tokens'].strip():missing+=1;continue
            values={k:int(row[col]) for col,k in fields.items()};total=int(row['Total Tokens'])
            if any(n<0 for n in values.values()) or sum(values.values())!=total:
                raise ValueError('Cursor counters do not reconcile')
            for k,n in values.items():daily[day][k]+=n
            daily[day]['total']+=total
    return {'source':'cursor-account','captured_at':capture_epoch(captured),'missing_rows':missing,'daily':[{'date':d,**v} for d,v in sorted(daily.items())]}
