"""fleet-install: the workspace binary list and the signature check."""
import importlib.machinery
import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def load():
    loader = importlib.machinery.SourceFileLoader("fleet_install", str(ROOT / "bin" / "fleet-install"))
    spec = importlib.util.spec_from_loader("fleet_install", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


def test_workspace_tools_lists_every_binary_with_its_package():
    tools = load().workspace_tools()
    assert tools["diskmap"] == "diskmap"
    assert tools["fleet-run"] == "fleet-run"
    assert tools["heavy-slot"] == "heavy-slot"
    assert tools["guard-push"] == "agentkit"


def test_team_is_read_from_the_identity_name():
    m = load()
    assert m.team_of("Developer ID Application: Someone (ABCDE12345)") == "ABCDE12345"
    assert m.team_of("-") == ""


def test_an_adhoc_binary_fails_the_check(tmp_path, monkeypatch):
    m = load()
    monkeypatch.setattr(m, "signature_info", lambda p: {
        "signed": True, "adhoc": True, "Identifier": "diskmap-61ec", "TeamIdentifier": "not set", "Signature": "adhoc"})
    ok, why = m.expected_ok(tmp_path / "diskmap", "diskmap", "org.example.fleet", "")
    assert not ok and "ad-hoc" in why


def test_the_stable_signature_passes_and_a_wrong_identifier_fails(tmp_path, monkeypatch):
    m = load()
    info = {"signed": True, "adhoc": False, "Identifier": "org.example.fleet.diskmap",
            "TeamIdentifier": "ABCDE12345", "Signature": ""}
    monkeypatch.setattr(m, "signature_info", lambda p: info)
    assert m.expected_ok(tmp_path / "x", "diskmap", "org.example.fleet", "ABCDE12345")[0]
    assert not m.expected_ok(tmp_path / "x", "placement", "org.example.fleet", "ABCDE12345")[0]
    assert not m.expected_ok(tmp_path / "x", "diskmap", "org.example.fleet", "ZZZZZ99999")[0]
