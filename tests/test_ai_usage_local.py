import json,time
from pathlib import Path
from fleet.ai_usage_local import seed,collect
from fleet.ai_usage import connect,read_summary


def setup(tmp_path):
    root=tmp_path/'logs';root.mkdir()
    config=tmp_path/'.config/fleet';config.mkdir(parents=True)
    (config/'ai-usage.json').write_text(json.dumps({'local_collection':{'enabled':True,'roots':[str(root)],'budget_seconds':5}}))
    records=tmp_path/'seed.jsonl'
    records.write_text(json.dumps(['old','req','2026-11-10T00:00:00Z','claude-test',1,2,3,4])+'\n')
    seed(tmp_path,[records])
    return root


def event(mid='new',out=2):
    return {'type':'assistant','requestId':'req','timestamp':'2026-11-17T01:00:00Z','message':{'id':mid,'model':'claude-test','usage':{'input_tokens':1,'output_tokens':out,'cache_creation_input_tokens':3,'cache_read_input_tokens':4},'content':'private text not stored'}}


def test_tail_replay_dedup_growth_and_deleted_source(tmp_path):
    root=setup(tmp_path);p=root/'session.jsonl';p.write_text(json.dumps(event())+'\n')
    assert collect(tmp_path)['affected_days']==1
    assert collect(tmp_path)['affected_days']==0
    with p.open('a') as f:f.write(json.dumps(event(out=5))+'\n')
    collect(tmp_path)
    with connect(tmp_path) as db:
        assert db.execute('select count(*) from requests').fetchone()[0]==2
        assert db.execute("select sum(total) from requests where day='2026-11-17'").fetchone()[0]==13
    p.unlink();collect(tmp_path)
    with connect(tmp_path) as db:assert db.execute('select count(*) from requests').fetchone()[0]==2
    assert 'private text' not in (tmp_path/'.local/state/fleet/ai-usage/daily.json').read_text()


def test_partial_line_not_lost_and_account_unchanged(tmp_path):
    root=setup(tmp_path);p=root/'session.jsonl';raw=json.dumps(event());p.write_text(raw[:30])
    assert collect(tmp_path)['status']=='partial'
    with p.open('a') as f:f.write(raw[30:]+'\n')
    collect(tmp_path)
    with connect(tmp_path) as db:
        assert db.execute('select count(*) from requests').fetchone()[0]==2
        assert db.execute("select count(*) from daily where source='codex-account'").fetchone()[0]==0


def test_baseline_required_and_disabled_no_sideeffects(tmp_path):
    assert collect(tmp_path)['status']=='disabled'
    root=setup(tmp_path)
    with connect(tmp_path) as db:db.execute('delete from collector_meta')
    assert collect(tmp_path)['status']=='needs-baseline'


def test_copy_and_stream_repetitions_are_not_added(tmp_path):
    root=setup(tmp_path)
    for name in ('a','b'):(root/(name+'.jsonl')).write_text((json.dumps(event())+'\n')*3)
    collect(tmp_path)
    with connect(tmp_path) as db:assert db.execute('select count(*) from requests').fetchone()[0]==2


def test_partial_seed_rejected_without_erasing_aggregate(tmp_path):
    import pytest
    from fleet.ai_usage import ingest
    root=setup(tmp_path)
    ingest(tmp_path,{'source':'claude-local','captured_at':time.time(),'daily':[{'date':'2026-11-10','total':999}]})
    bad=tmp_path/'bad.jsonl';bad.write_text(json.dumps(['tiny','req','2026-11-10T00:00:00Z','claude-test',1,1,1,1])+'\n')
    with pytest.raises(ValueError,match='undercovers'):seed(tmp_path,[bad])
    with connect(tmp_path) as db:
        assert json.loads(db.execute("select payload from daily where day='2026-11-10'").fetchone()[0])['total']==999
        assert db.execute('select count(*) from requests').fetchone()[0]==1
