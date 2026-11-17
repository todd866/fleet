import json
import pytest
from fleet.ai_usage import ingest, refresh, read_summary, connect, validate
from fleet.morning.evaluate import _ai_usage


def doc(source='codex-account', captured=1794900000, value=12):
    return dict(source=source, captured_at=captured, daily=[dict(date='2026-11-15',total=value)])


def test_idempotent_import_and_null_categories(tmp_path):
    ingest(tmp_path,doc()); ingest(tmp_path,doc())
    with connect(tmp_path) as db:
        assert db.execute('select count(*) from daily').fetchone()[0] == 1
    daily=json.loads((tmp_path/'.local/state/fleet/ai-usage/daily.json').read_text())['daily']
    assert daily[0]['cache_read'] is None
    assert len(list((tmp_path/'.local/state/fleet/ai-usage/imports').glob('*.json'))) == 1


def test_distinct_scopes_never_added(tmp_path):
    ingest(tmp_path,doc()); ingest(tmp_path,doc('codex-local',value=3))
    assert len(read_summary(tmp_path)['sources']) == 2


def test_reject_stale_invalid_and_strip_secrets(tmp_path):
    d=doc(); d['cookie']='secret';d['daily'][0]['prompt']='private'
    ingest(tmp_path,d)
    assert 'secret' not in next((tmp_path/'.local/state/fleet/ai-usage/imports').glob('*.json')).read_text()
    with pytest.raises(ValueError): ingest(tmp_path,doc(captured=1794800000))
    d=doc();d['daily'][0]['total']=-1
    with pytest.raises(ValueError): validate(d)


def test_digest_freshness_uses_capture_not_regeneration(tmp_path):
    (tmp_path/'.local/state/mac-sync').mkdir(parents=True)
    (tmp_path/'.config/fleet').mkdir(parents=True)
    (tmp_path/'.config/fleet/ai-usage.json').write_text('{"publish_shared":true}')
    ingest(tmp_path,doc());summary=refresh(tmp_path,now=1795500000)
    assert _ai_usage(summary,1795500000)[0].status == 'warn'
    assert _ai_usage(summary,1794900000)[0].status == 'pass'
    assert (tmp_path/'.local/state/mac-sync/fleet-ai-usage/summary.json').exists()


def test_partial_export_does_not_erase_history(tmp_path):
    ingest(tmp_path,doc())
    d=doc(captured=1794900100);d['daily'][0]['date']='2026-11-16';ingest(tmp_path,d)
    with connect(tmp_path) as db: assert db.execute('select count(*) from daily').fetchone()[0] == 2


def test_empty_reader_cannot_replace_shared_history(tmp_path):
    shared=tmp_path/'.local/state/mac-sync/fleet-ai-usage/summary.json'
    shared.parent.mkdir(parents=True)
    shared.write_text(json.dumps({'schema':1,'generated_at':1,'sources':[{'source':'codex-account'}]}))
    refresh(tmp_path,now=2)
    assert read_summary(tmp_path)['sources'] == [{'source':'codex-account'}]
    assert json.loads(shared.read_text())['generated_at'] == 1


def test_bad_summary_does_not_crash_digest(tmp_path):
    path=tmp_path/'.local/state/fleet/ai-usage/summary.json'
    path.parent.mkdir(parents=True);path.write_text('[]')
    assert read_summary(tmp_path) is None
    assert _ai_usage({'sources':[None]},1794900000)[0].status == 'warn'
