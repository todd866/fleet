"""Incremental Claude-format usage metadata. Bounded producer, never a browser scraper."""
import fcntl
import gzip
import hashlib
import json
import time
from datetime import datetime, timezone
from pathlib import Path

from fleet.ai_usage import CATEGORIES, SOURCES, atomic, connect, refresh, state_dir


def settings(home):
    try:
        value = json.loads((Path(home) / '.config/fleet/ai-usage.json').read_text()).get('local_collection', {})
        return value if isinstance(value, dict) else {}
    except (ValueError, OSError, AttributeError):
        return {}


def schema(db):
    db.execute('CREATE TABLE IF NOT EXISTS requests (id TEXT PRIMARY KEY, source TEXT, day TEXT, input INTEGER, output INTEGER, read INTEGER, write INTEGER, total INTEGER)')
    db.execute('CREATE TABLE IF NOT EXISTS offsets (path TEXT PRIMARY KEY, inode INTEGER, offset INTEGER, mtime INTEGER)')
    db.execute('CREATE TABLE IF NOT EXISTS collector_meta (key TEXT PRIMARY KEY, value TEXT)')
    db.execute('CREATE INDEX IF NOT EXISTS requests_day ON requests(source,day)')


def upsert(db, record, mapping):
    """Metadata tuple: message ID, request ID, timestamp, model, input, output, write, read."""
    if not isinstance(record, list) or len(record) != 8:
        return None
    mid, req, stamp, model, inp, out, write, read = record
    if not mid or not isinstance(model, str) or any(type(n) is not int or n < 0 for n in (inp,out,write,read)):
        return None
    source = next((value for prefix, value in mapping.items() if model.startswith(prefix) and value in SOURCES and SOURCES[value][1] == 'local'), None)
    if source is None:
        return None
    try:
        day = datetime.fromisoformat(stamp.replace('Z', '+00:00')).astimezone(timezone.utc).date().isoformat()
    except (ValueError, AttributeError):
        return None
    key = hashlib.sha256(json.dumps([mid,req],separators=(',',':')).encode()).hexdigest()
    total = inp+out+write+read
    before = db.execute('SELECT source,day,total FROM requests WHERE id=?',(key,)).fetchone()
    if before and before[2] >= total:
        return None
    db.execute('INSERT OR REPLACE INTO requests VALUES (?,?,?,?,?,?,?,?)',(key,source,day,inp,out,read,write,total))
    return {(source,day)} | ({(before[0],before[1])} if before else set())


def rebuild(db, affected, captured):
    for source,day in affected:
        counts = db.execute('SELECT sum(input),sum(output),sum(read),sum(write),sum(total) FROM requests WHERE source=? AND day=?',(source,day)).fetchone()
        if counts[-1] is None:
            continue
        row = dict(zip(('uncached_input','output','cache_read','cache_write','total'),counts))
        row.update(date=day,unclassified_total_difference=None)
        db.execute('INSERT OR REPLACE INTO daily VALUES (?,?,?)',(source,day,json.dumps(row)))
    for source in {s for s,_ in affected}:
        span=db.execute('SELECT min(day),max(day) FROM daily WHERE source=?',(source,)).fetchone()
        previous=db.execute('SELECT captured FROM sources WHERE source=?',(source,)).fetchone()
        clock=captured if captured is not None else previous[0] if previous else 0
        db.execute('INSERT OR REPLACE INTO sources VALUES (?,?,?,?,?)',(source,clock,*span,0))


def seed(home, paths):
    """One-time metadata baseline; global IDs deduplicate backups and streaming repeats."""
    cfg=settings(home);mapping=cfg.get('model_sources', {'claude-':'claude-local','deepseek':'deepseek-local'})
    affected=set()
    with connect(home) as db:
        schema(db)
        for path in paths:
            opener=gzip.open if str(path).endswith('.gz') else open
            with opener(path,'rt') as handle:
                for line in handle:
                    changed=upsert(db,json.loads(line),mapping)
                    if changed:affected.update(changed)
        # Reject partial seeds before changing any retained aggregate. Transaction rolls back.
        for source,day,payload in db.execute('SELECT source,day,payload FROM daily').fetchall():
            if source not in mapping.values():continue
            old=json.loads(payload)
            counters=db.execute('SELECT sum(input),sum(output),sum(read),sum(write),sum(total) FROM requests WHERE source=? AND day=?',(source,day)).fetchone()
            for key,count in zip(('uncached_input','output','cache_read','cache_write','total'),counters):
                if old.get(key) is not None and (count is None or count<old[key]):
                    raise ValueError('Baseline undercovers retained usage; provide all deduplicated request metadata before enabling collection')
        if not db.execute('SELECT 1 FROM requests LIMIT 1').fetchone():
            raise ValueError('Empty usage metadata baseline')
        rebuild(db,affected,time.time())
        db.execute('INSERT OR REPLACE INTO collector_meta VALUES (?,?)',('seeded','true'))
    return refresh(home)


def collect(home):
    cfg=settings(home)
    if cfg.get('enabled') is not True:
        return {'status':'disabled'}
    root=state_dir(home);root.mkdir(parents=True,exist_ok=True)
    with (root/'collector.lock').open('a') as lock:
        try:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:return {'status':'busy'}
        return _collect(home,cfg)


def _collect(home,cfg):
    started=time.monotonic();budget=min(30,max(.1,float(cfg.get('budget_seconds',5))));cap=min(256*1024*1024,max(1024,int(cfg.get('max_bytes',64*1024*1024))))
    mapping=cfg.get('model_sources', {'claude-':'claude-local','deepseek':'deepseek-local'})
    affected=set();read_bytes=0;files=0;errors=0;pending=False
    with connect(home) as db:
        schema(db)
        if not db.execute("SELECT 1 FROM collector_meta WHERE key='seeded'").fetchone():
            result={'status':'needs-baseline','checked_at':time.time(),'error':'Seed usage metadata before enabling collection; existing aggregate totals must not be replaced by a partial scan.'}
            atomic(state_dir(home)/'collector.json',result);return result
        roots=cfg.get('roots',[str(Path(home)/'.claude/projects')])
        for directory in roots:
            folder=Path(directory).expanduser()
            if not folder.is_dir():errors+=1;continue
            for path in folder.rglob('*.jsonl'):
                if time.monotonic()-started>=budget or read_bytes>=cap:pending=True;break
                try:
                    stat=path.stat();old=db.execute('SELECT inode,offset,mtime FROM offsets WHERE path=?',(str(path),)).fetchone()
                    offset=old[1] if old and old[0]==stat.st_ino and old[1]<=stat.st_size else 0
                    if old and offset==stat.st_size and old[2]==stat.st_mtime_ns:continue
                    if old and offset==stat.st_size and old[2]!=stat.st_mtime_ns:offset=0
                    files+=1
                    with path.open('rb') as handle:
                        handle.seek(offset)
                        while time.monotonic()-started<budget and read_bytes<cap:
                            begin=handle.tell();line=handle.readline(16*1024*1024)
                            if not line:break
                            read_bytes+=len(line)
                            if not line.endswith(b'\n'):
                                handle.seek(begin);pending=True
                                if len(line)>=16*1024*1024:errors+=1
                                break
                            try:
                                item=json.loads(line)
                                if isinstance(item,dict) and item.get('type')=='assistant':
                                    m=item.get('message') or {};u=m.get('usage') or {}
                                    if u:
                                        record=[m.get('id') or item.get('uuid'),item.get('requestId'),item.get('timestamp'),m.get('model'),*[u.get(k,0) for k in ('input_tokens','output_tokens','cache_creation_input_tokens','cache_read_input_tokens')]]
                                        changed=upsert(db,record,mapping)
                                        if changed:affected.update(changed)
                            except (ValueError,TypeError,AttributeError):errors+=1
                            offset=handle.tell()
                        if handle.tell()<stat.st_size:pending=True
                    db.execute('INSERT OR REPLACE INTO offsets VALUES (?,?,?,?)',(str(path),stat.st_ino,offset,stat.st_mtime_ns))
                except OSError:errors+=1
            if pending and (time.monotonic()-started>=budget or read_bytes>=cap):break
        rebuild(db,affected,None)
    result={'status':'partial' if pending or errors else 'ok','sources':sorted(set(mapping.values())),'checked_at':time.time(),'files_read':files,'bytes_read':read_bytes,'affected_days':len(affected),'errors':errors,'elapsed_seconds':round(time.monotonic()-started,3)}
    atomic(state_dir(home)/'collector.json',result)
    refresh(home)
    return result
