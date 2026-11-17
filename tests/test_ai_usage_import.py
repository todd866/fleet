import json
import pytest
from fleet.ai_usage_import import codex,cursor,capture_epoch


def test_codex_reconcile_and_zero_dates(tmp_path):
    p=tmp_path/'c.json';v={'daily':[['2026-01-01',3]],'lifetime_tokens':3,'coverage_start':'2026-01-01','coverage_end':'2026-01-02'};p.write_text(json.dumps(v))
    assert codex(p,'2026-01-03T00:00:00Z')['daily'][1]['total']==0
    v['lifetime_tokens']=4;p.write_text(json.dumps(v))
    with pytest.raises(ValueError):codex(p,'2026-01-03T00:00:00Z')


def test_cursor_unknown_rows_and_reconciliation(tmp_path):
    p=tmp_path/'c.csv';p.write_text('Date,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens\n2026-01-01T12:00:00Z,1,2,3,4,10\n2026-01-01T13:00:00Z,,,,,\n')
    d=cursor(p,'2026-01-02T00:00:00Z');assert d['missing_rows']==1;assert d['daily'][0]['total']==10
    with pytest.raises(ValueError):capture_epoch('2026-01-01T00:00:00')
