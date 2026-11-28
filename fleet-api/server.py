#!/usr/bin/env python3
"""Authenticated tailnet-only JSON API and OTA server for the Fleet iOS app."""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import hashlib
import hmac
import html
import ipaddress
import json
import math
import os
import plistlib
import re
import shlex
import shutil
import socket
import subprocess
import sys
import threading
import time
import urllib.parse
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable

API_VERSION = 1
MAX_BODY_BYTES = 16 * 1024
MAX_TEXT_LEN = 2000
MAIL_TARGET_RE = re.compile(r"^[a-z0-9_-]+(:[a-z0-9_.-]+){0,2}$")
HOST_ID_RE = re.compile(r"^[a-z0-9_-]{1,32}$")
JOB_LABEL_RE = re.compile(r"^[A-Za-z0-9._-]{3,128}$")
HOST_ACTION_SUFFIXES = {
    "free-space-guard": "free-space-guard",
    "snapshot": "fleet-snapshot",
    "idrive-tick": "idrive-ingest",
    "placement-check": "placement-status,storage-health,fleet-snapshot",
}
ALLOWED_PAUSE_PROVIDERS = frozenset({"claude", "codex", "zai", "cursor"})
_BAD_EXIT_VALUES = {None, "", "-", "0", 0}
_TERM_EXIT_VALUES = {"-15", "143"}

DEFAULT_CONFIG: dict[str, Any] = {
    "bind": "127.0.0.1",
    "port": 8694,
    "public_base": "https://hub.example.ts.net:8444",
    "token_file": "~/.config/fleet/fleet-api-token",
    "ota_dir": "~/.local/state/fleet-api/ota",
    "audit_log": "~/.local/state/fleet-api/actions.jsonl",
    "bundle_id": "org.example.fleet",
    "app_title": "Fleet",
    "home_dir": "~",
    "mac_sync_dir": "~/.local/state/mac-sync",
    "evidence_dir": "~/.local/state/fleet/evidence",
    "agent_mail_bin": "~/.local/bin/agent-mail",
    "fleet_bin": "",
    "mail_ack_file": "~/.local/state/fleet-api/mail-ack.json",
    "snooze_file": "~/.local/state/fleet-api/snoozed.json",
    "site_config": "~/.config/fleet/site.json",
    "drive_places_file": "~/.config/fleet/drive-places.json",
    "drive_plan_file": "~/.local/state/fleet-api/drive-plan.json",
    "music_stations_file": "~/Projects/music-radio/data/stations.json",
    "remote_action_hosts": [],
    "read_rate_limit_per_min": 60,
    "write_rate_limit_per_min": 20,
}


def _expand(raw: str | Path) -> Path:
    return Path(os.path.expanduser(str(raw)))


def _utc_iso(epoch: float | int | None = None) -> str:
    t = time.time() if epoch is None else float(epoch)
    return dt.datetime.fromtimestamp(t, tz=dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _read_json(path: Path, default: Any = None) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8", errors="replace"))
    except (OSError, ValueError):
        return default


def _read_text(path: Path, default: str = "") -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return default


def _read_jsonl(path: Path, limit: int = 200) -> list[dict[str, Any]]:
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()[-limit:]
    except OSError:
        return []
    out: list[dict[str, Any]] = []
    for line in lines:
        try:
            obj = json.loads(line)
        except ValueError:
            continue
        if isinstance(obj, dict):
            out.append(obj)
    return out


def _gb(val: Any) -> float | None:
    if isinstance(val, (int, float)):
        return round(float(val) / 1e9, 2)
    return None


def load_config(path: Path | None = None) -> dict[str, Any]:
    cfg_path = path or _expand(os.environ.get("FLEET_API_CONFIG") or "~/.config/fleet/fleet-api.json")
    cfg = dict(DEFAULT_CONFIG)
    if cfg_path.is_file():
        over = _read_json(cfg_path, {})
        if isinstance(over, dict):
            cfg.update({k: v for k, v in over.items() if v is not None})
    repo_fleet = Path(__file__).resolve().parents[1] / "bin" / "fleet"
    if not cfg.get("fleet_bin"):
        cfg["fleet_bin"] = str(repo_fleet if repo_fleet.is_file() else (_expand("~/.local/bin/fleet")))
    return cfg


def load_token(cfg: dict[str, Any]) -> str:
    env_tok = os.environ.get("FLEET_API_TOKEN", "").strip()
    if env_tok:
        return env_tok
    tf = _expand(cfg["token_file"])
    if tf.is_file():
        tok = tf.read_text(encoding="utf-8").strip()
        if tok:
            return tok
    raise RuntimeError(f"Fleet API token missing at {tf}")


class RateLimiter:
    def __init__(self, limit_per_min: int) -> None:
        self.limit = max(1, int(limit_per_min))
        self._hits: dict[str, collections.deque[float]] = {}
        self._lock = threading.Lock()

    def allow(self, key: str) -> bool:
        now = time.monotonic()
        cutoff = now - 60.0
        with self._lock:
            q = self._hits.setdefault(key, collections.deque())
            while q and q[0] < cutoff:
                q.popleft()
            if len(q) >= self.limit:
                return False
            q.append(now)
            return True


def verify_token(supplied: str, expected: str) -> bool:
    if not supplied or not expected or len(supplied) < 16:
        return False
    return hmac.compare_digest(supplied.encode("utf-8"), expected.encode("utf-8"))


def _parse_mail_epoch(raw: Any) -> float | None:
    if isinstance(raw, bool) or raw is None:
        return None
    if isinstance(raw, (int, float)):
        val = float(raw)
        return val if val > 1_000_000_000 else None
    text = str(raw).strip()
    if not text:
        return None
    try:
        val = float(text)
        return val if val > 1_000_000_000 else None
    except ValueError:
        pass
    try:
        iso = text[:-1] + "+00:00" if text.endswith("Z") else text
        parsed = dt.datetime.fromisoformat(iso)
        return parsed.timestamp()
    except ValueError:
        pass
    try:
        return dt.datetime.strptime(text, "%Y-%m-%d %H:%M:%S").timestamp()
    except ValueError:
        return None


_REMOTE_JOB_SCRIPT = r"""
import json, os, plistlib, re, subprocess, sys, time
from pathlib import Path

mode = sys.argv[1]
arg = sys.argv[2]
extra = sys.argv[3]
allowed_namespace = re.compile(sys.argv[4], re.IGNORECASE).match
home = Path.home()
agents = home / "Library/LaunchAgents"
uid = os.getuid()

def parse_launchctl(label):
    res = subprocess.run(["launchctl", "print", f"gui/{uid}/{label}"], capture_output=True, text=True, timeout=6)
    text = (res.stdout or "") + "\n" + (res.stderr or "")
    state = None
    last_exit = None
    for line in text.splitlines():
        s = line.strip()
        if s.startswith("state = "):
            state = s.split("=", 1)[1].strip()
        elif s.startswith("last exit code = "):
            raw = s.split("=", 1)[1].strip()
            try:
                last_exit = int(raw)
            except ValueError:
                last_exit = raw
    return res.returncode, state, last_exit, text.strip()

def read_tail(raw_path, lines):
    if not raw_path:
        return None, ""
    p = Path(os.path.expanduser(str(raw_path))).resolve()
    allowed_roots = (home.resolve(), Path("/tmp").resolve(), Path("/private/tmp").resolve(), Path("/var/log").resolve(), Path("/Library/Logs").resolve())
    if not any(p == r or str(p).startswith(str(r) + "/") for r in allowed_roots):
        return str(p), "[log path outside allowed roots]"
    if not p.is_file():
        return str(p), ""
    try:
        size = p.stat().st_size
        with p.open("rb") as fh:
            if size > 65536:
                fh.seek(-65536, os.SEEK_END)
            data = fh.read().decode("utf-8", errors="replace")
        tail = "\n".join(data.splitlines()[-lines:])
        if len(tail) > 12288:
            tail = tail[-12288:]
        return str(p), tail
    except OSError as e:
        return str(p), f"[unreadable: {e}]"

if mode == "job-log":
    lines = max(10, min(100, int(extra or "60")))
    plist_path = agents / f"{arg}.plist"
    if not plist_path.is_file():
        print(json.dumps({"ok": False, "code": 404, "error": f"plist missing: {plist_path.name}"}))
        sys.exit(0)
    pl = plistlib.loads(plist_path.read_bytes())
    err_path, err_txt = read_tail(pl.get("StandardErrorPath"), lines)
    out_raw = pl.get("StandardOutPath")
    if out_raw and os.path.expanduser(str(out_raw)) == os.path.expanduser(str(pl.get("StandardErrorPath") or "")):
        out_path, out_txt = err_path, ""
    else:
        out_path, out_txt = read_tail(out_raw, lines)
    rc, state, last_exit, _ = parse_launchctl(arg)
    print(json.dumps({
        "ok": True,
        "label": arg,
        "stderr_path": err_path,
        "stdout_path": out_path,
        "stderr": err_txt,
        "stdout": out_txt,
        "state": state or ("unloaded" if rc != 0 else "unknown"),
        "last_exit": last_exit,
    }))
elif mode in ("job-kickstart", "host-action"):
    if mode == "host-action":
        suffixes = [s for s in arg.split(",") if s]
        matches = []
        for sfx in suffixes:
            found = sorted(p for p in agents.glob(f"*.{sfx}.plist") if allowed_namespace(p.stem))
            if found:
                matches = found
                break
        if not matches:
            print(json.dumps({"ok": False, "code": 404, "error": f"no plist matching .{arg}.plist on host"}))
            sys.exit(0)
        plist_path = matches[0]
        label = plist_path.stem
    else:
        label = arg
        plist_path = agents / f"{label}.plist"
        if not plist_path.is_file():
            print(json.dumps({"ok": False, "code": 404, "error": f"plist missing: {plist_path.name}"}))
            sys.exit(0)
    k = subprocess.run(["launchctl", "kickstart", "-k", f"gui/{uid}/{label}"], capture_output=True, text=True, timeout=10)
    time.sleep(0.8)
    _, state, last_exit, _ = parse_launchctl(label)
    if mode == "job-kickstart":
        snapshots = sorted(p for p in agents.glob("*.fleet-snapshot.plist") if allowed_namespace(p.stem))
        for snap_p in snapshots[:1]:
            if snap_p.stem != label:
                subprocess.Popen(["launchctl", "kickstart", f"gui/{uid}/{snap_p.stem}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if k.returncode != 0:
        err = (k.stderr or k.stdout or f"kickstart exit {k.returncode}").strip()[:200]
        print(json.dumps({"ok": False, "code": 409, "label": label, "error": err, "state": state, "last_exit": last_exit}))
    else:
        print(json.dumps({
            "ok": True,
            "label": label,
            "state": state or "running",
            "last_exit": last_exit,
            "detail": f"Kickstarted {label} ({state or 'running'})",
        }))
"""


class FleetDataStore:
    """Reads synced Fleet state files directly from disk without spawning subprocesses."""

    def __init__(self, cfg: dict[str, Any]) -> None:
        self.cfg = cfg
        self.home = _expand(cfg["home_dir"])
        self.mac_sync = _expand(cfg["mac_sync_dir"])
        self.evidence_dir = _expand(cfg["evidence_dir"])
        self.ota_dir = _expand(cfg["ota_dir"])
        self.audit_log = _expand(cfg["audit_log"])
        self.mail_ack_file = _expand(cfg.get("mail_ack_file") or "~/.local/state/fleet-api/mail-ack.json")
        self.snooze_file = _expand(cfg.get("snooze_file") or "~/.local/state/fleet-api/snoozed.json")
        self.site_config_path = _expand(cfg.get("site_config") or "~/.config/fleet/site.json")
        self.drive_places_file = _expand(cfg.get("drive_places_file") or "~/.config/fleet/drive-places.json")
        self.drive_plan_file = _expand(cfg.get("drive_plan_file") or "~/.local/state/fleet-api/drive-plan.json")
        self.music_stations_file = _expand(cfg.get("music_stations_file") or "~/Projects/music-radio/data/stations.json")
        self.started_at = time.monotonic()
        self._refresh_lock = threading.Lock()
        self._refresh_running = False
        self._mail_ack_lock = threading.Lock()
        self._snooze_lock = threading.Lock()
        self._drive_lock = threading.Lock()
        self._drive_shuffle_idx = 0

    def _site_meta(self) -> dict[str, Any]:
        doc = _read_json(self.site_config_path, {})
        return doc if isinstance(doc, dict) else {}

    def _hub_id(self) -> str:
        raw = str(self._site_meta().get("hub") or socket.gethostname().split(".")[0]).strip().lower()
        return raw if HOST_ID_RE.match(raw) else "hub"

    def _action_hosts(self) -> list[str]:
        out: list[str] = []
        hub = self._hub_id()
        if HOST_ID_RE.match(hub):
            out.append(hub)
        raw_remote = self.cfg.get("remote_action_hosts") or []
        if isinstance(raw_remote, list):
            for item in raw_remote:
                hid = str(item or "").strip().lower()
                if HOST_ID_RE.match(hid) and hid not in out:
                    out.append(hid)
        return out

    def _job_namespace_pattern(self) -> str:
        prefixes = self._site_meta().get("job_label_prefixes")
        names = []
        for raw in prefixes if isinstance(prefixes, list) else []:
            prefix = raw.strip().lower() if isinstance(raw, str) else ""
            if re.fullmatch(r"[a-z0-9_-]+(?:\.[a-z0-9_-]+)*", prefix):
                # Short site keys also match the second component of a launchd label.
                first = r"(?:[a-z0-9_-]+\.)?" if "." not in prefix else ""
                names.append(first + re.escape(prefix))
        return r"^(?:" + "|".join(names) + r")\." if names else ""

    def _is_allowed_job_label(self, label: str) -> bool:
        namespace = self._job_namespace_pattern()
        return bool(namespace and JOB_LABEL_RE.fullmatch(label) and ".." not in label
                    and re.match(namespace, label, re.IGNORECASE))

    def _load_snoozes(self) -> dict[str, dict[str, Any]]:
        doc = _read_json(self.snooze_file, {})
        if not isinstance(doc, dict):
            return {}
        raw_items = doc.get("items")
        if not isinstance(raw_items, dict):
            return {}
        now = time.time()
        clean: dict[str, dict[str, Any]] = {}
        for k, v in raw_items.items():
            if not isinstance(k, str) or not isinstance(v, dict):
                continue
            try:
                until_epoch = float(v.get("until_epoch") or 0.0)
            except (TypeError, ValueError):
                continue
            if until_epoch > now:
                clean[k] = {
                    "until_epoch": until_epoch,
                    "until": str(v.get("until") or _utc_iso(until_epoch)),
                    "note": str(v.get("note") or ""),
                }
        return clean

    def _save_snoozes(self, items: dict[str, dict[str, Any]]) -> None:
        self.snooze_file.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.snooze_file.with_name(f".{self.snooze_file.name}.{os.getpid()}.tmp")
        tmp.write_text(json.dumps({"items": items}, separators=(",", ":")), encoding="utf-8")
        os.replace(tmp, self.snooze_file)

    def _annotate_rows(
        self,
        raw_rows: list[Any],
        base_counts: dict[str, Any] | None,
    ) -> tuple[list[dict[str, Any]], dict[str, int], dict[str, str]]:
        snoozes = self._load_snoozes()
        rows_out: list[dict[str, Any]] = []
        raw_counts = {"pass": 0, "warn": 0, "fail": 0}
        snoozed_delta = {"warn": 0, "fail": 0}
        active_snoozed: dict[str, str] = {}
        for item in raw_rows:
            if not isinstance(item, dict):
                continue
            row = dict(item)
            key = str(row.get("key") or "")
            st = str(row.get("status") or "").lower()
            if st in ("pass", "ok", "healthy"):
                raw_counts["pass"] += 1
            elif st in ("warn", "warning", "stale"):
                raw_counts["warn"] += 1
            elif st in ("fail", "error", "crit", "critical"):
                raw_counts["fail"] += 1
            snz = snoozes.get(key)
            if snz:
                row["snoozed_until"] = snz["until"]
                active_snoozed[key] = snz["until"]
                if st in ("warn", "warning", "stale"):
                    snoozed_delta["warn"] += 1
                elif st in ("fail", "error", "crit", "critical"):
                    snoozed_delta["fail"] += 1
            else:
                row["snoozed_until"] = None
            rows_out.append(row)

        if isinstance(base_counts, dict):
            counts = {
                "pass": int(base_counts.get("pass") or 0),
                "warn": max(0, int(base_counts.get("warn") or 0) - snoozed_delta["warn"]),
                "fail": max(0, int(base_counts.get("fail") or 0) - snoozed_delta["fail"]),
            }
        else:
            counts = {
                "pass": raw_counts["pass"],
                "warn": max(0, raw_counts["warn"] - snoozed_delta["warn"]),
                "fail": max(0, raw_counts["fail"] - snoozed_delta["fail"]),
            }
        return rows_out, counts, active_snoozed

    def _run_host_job_script(self, host: str, mode: str, arg: str, extra: str = "") -> tuple[int, dict[str, Any]]:
        if host not in self._action_hosts():
            return 403, {"ok": False, "error": f"actions not permitted on host {host!r}"}
        namespace = self._job_namespace_pattern()
        if not namespace or (mode != "host-action" and not re.match(namespace, arg, re.IGNORECASE)):
            return 403, {"ok": False, "error": "job outside configured namespaces"}
        params = [mode, arg, extra, namespace]
        hub = self._hub_id()
        try:
            if host == hub:
                env = dict(os.environ)
                env["HOME"] = str(self.home)
                res = subprocess.run(
                    [sys.executable, "-c", _REMOTE_JOB_SCRIPT, *params],
                    capture_output=True,
                    text=True,
                    env=env,
                    timeout=20,
                )
            else:
                res = subprocess.run(
                    ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=6", "--", host, shlex.join(["python3", "-", *params])],
                    input=_REMOTE_JOB_SCRIPT,
                    capture_output=True,
                    text=True,
                    timeout=22,
                )
        except subprocess.TimeoutExpired:
            return 504, {"ok": False, "error": f"timed out contacting host {host}"}
        if res.returncode != 0 and not res.stdout.strip():
            err = (res.stderr or f"exit {res.returncode}").strip()[:200]
            return 502, {"ok": False, "error": err}
        try:
            doc = json.loads(res.stdout.strip())
        except ValueError:
            return 502, {"ok": False, "error": f"invalid response from {host}: {res.stdout[:120]}"}
        if not isinstance(doc, dict):
            return 502, {"ok": False, "error": "unexpected host response"}
        code = int(doc.pop("code", 200 if doc.get("ok") else 400))
        doc["host"] = host
        return code, doc

    def _send_agent_mail(self, to: str, text: str) -> tuple[bool, str, int]:
        raw_tool = str(self.cfg.get("agent_mail_bin") or "").strip()
        if not raw_tool:
            return False, "agent-mail not configured", 500
        tool = _expand(raw_tool)
        if not tool.is_file() and raw_tool == DEFAULT_CONFIG["agent_mail_bin"]:
            found = shutil.which("agent-mail")
            if found:
                tool = Path(found)
        if not tool.is_file():
            return False, "agent-mail not installed", 500
        res = subprocess.run([str(tool), "send", to, text], capture_output=True, text=True, timeout=15)
        if res.returncode != 0:
            return False, (res.stderr.strip()[:200] or f"agent-mail exit {res.returncode}"), 502
        return True, res.stdout.strip(), 200

    def _load_mail_ack(self) -> dict[str, Any]:
        doc = _read_json(self.mail_ack_file, {})
        if not isinstance(doc, dict):
            doc = {}
        raw_ids = doc.get("acked_ids")
        acked_ids = [str(x) for x in raw_ids if isinstance(x, str) and x] if isinstance(raw_ids, list) else []
        try:
            acked_epoch = float(doc.get("acked_epoch") or 0.0)
        except (TypeError, ValueError):
            acked_epoch = 0.0
        return {
            "acked_epoch": acked_epoch,
            "acked_at": doc.get("acked_at") if isinstance(doc.get("acked_at"), str) else None,
            "acked_ids": acked_ids[-2000:],
        }

    def _save_mail_ack(self, ack: dict[str, Any]) -> None:
        self.mail_ack_file.parent.mkdir(parents=True, exist_ok=True)
        seen: set[str] = set()
        deduped: list[str] = []
        for mid in ack.get("acked_ids") or []:
            if mid and mid not in seen:
                seen.add(mid)
                deduped.append(mid)
        out = {
            "acked_epoch": float(ack.get("acked_epoch") or 0.0),
            "acked_at": ack.get("acked_at"),
            "acked_ids": deduped[-2000:],
        }
        tmp = self.mail_ack_file.with_name(f".{self.mail_ack_file.name}.{os.getpid()}.tmp")
        tmp.write_text(json.dumps(out, separators=(",", ":")), encoding="utf-8")
        os.replace(tmp, self.mail_ack_file)

    @staticmethod
    def _is_msg_acked(msg_id: Any, raw_at: Any, ack: dict[str, Any], ack_ids_set: set[str]) -> bool:
        if isinstance(msg_id, str) and msg_id in ack_ids_set:
            return True
        acked_epoch = float(ack.get("acked_epoch") or 0.0)
        if acked_epoch > 0.0:
            ep = _parse_mail_epoch(raw_at)
            if ep is not None and ep <= acked_epoch:
                return True
        return False

    def _morning_summary(self) -> dict[str, Any]:
        for cand in (
            self.mac_sync / "fleet-morning" / "summary.json",
            self.home / ".local/state/fleet/morning/summary.json",
        ):
            doc = _read_json(cand)
            if isinstance(doc, dict) and "rows" in doc:
                return doc
        return {}

    def _agy_morning_text(self) -> str | None:
        synced = self.mac_sync / "fleet-morning" / "agy-morning.md"
        if synced.is_file():
            txt = _read_text(synced).strip()
            if txt:
                return txt
        local_dir = self.home / ".local/state/agy-morning"
        if local_dir.is_dir():
            files = sorted(local_dir.glob("20*.md"), key=lambda p: p.stat().st_mtime, reverse=True)
            if files:
                txt = _read_text(files[0]).strip()
                if txt:
                    return txt
        return None

    def _host_snapshots(self) -> dict[str, dict[str, Any]]:
        snap_dir = self.mac_sync / "fleet-snapshots"
        out: dict[str, dict[str, Any]] = {}
        if snap_dir.is_dir():
            for p in sorted(snap_dir.glob("*.json")):
                doc = _read_json(p)
                if isinstance(doc, dict):
                    hid = str(doc.get("host") or p.stem)
                    out[hid] = doc
        return out

    def _paused_providers(self) -> list[str]:
        p = self.home / ".config/agentkit/paused-providers"
        if not p.is_file():
            return []
        paused: list[str] = []
        for line in _read_text(p).splitlines():
            tok = line.split("#", 1)[0].strip().lower()
            if tok and tok not in paused:
                paused.append(tok)
        return paused

    def _disk_targets(self) -> dict[str, dict[str, float]]:
        p = self.mac_sync / "storage-audit" / "free-targets.tsv"
        out: dict[str, dict[str, float]] = {}
        if not p.is_file():
            return out
        for line in _read_text(p).splitlines():
            if not line.strip() or line.startswith("#"):
                continue
            parts = [x.strip() for x in line.split("\t")]
            if len(parts) < 4:
                continue
            try:
                out[parts[0]] = {
                    "target_gb": float(parts[1]),
                    "with_purgeable_gb": float(parts[2]),
                    "floor_gb": float(parts[3]),
                }
            except ValueError:
                continue
        return out

    def build_hosts(self) -> dict[str, Any]:
        snaps = self._host_snapshots()
        summary = self._morning_summary()
        targets = self._disk_targets()
        rows = summary.get("rows") or []

        hosts_out: list[dict[str, Any]] = []
        for hid in sorted(snaps.keys()):
            doc = snaps[hid]
            r = doc.get("reading") or {}
            samples = r.get("samples") or []
            latest = samples[-1] if samples else {}
            disks = r.get("disks") or []
            internal = next((d for d in disks if d.get("name") == "internal"), {})
            free_gb = _gb(internal.get("free_bytes"))
            if free_gb is None and isinstance(latest.get("diskFreeGB"), (int, float)):
                free_gb = round(float(latest["diskFreeGB"]), 1)
            total_gb = _gb(internal.get("total_bytes"))
            if total_gb is None and isinstance(latest.get("diskTotalGB"), (int, float)):
                total_gb = round(float(latest["diskTotalGB"]), 1)
            tinfo = targets.get(hid, {})

            host_rows = [
                row for row in rows
                if isinstance(row, dict)
                and (
                    str(row.get("key", "")).startswith(f"job:{hid}:")
                    or str(row.get("key", "")).startswith(f"disk:{hid}:")
                    or str(row.get("key", "")).startswith(f"storage:{hid}:")
                    or str(row.get("key", "")).startswith(f"thermal:{hid}")
                    or str(row.get("key", "")).startswith(f"backup:{hid}:")
                    or str(row.get("key", "")).startswith(f"kit:{hid}:")
                )
            ]
            jobs = r.get("jobs") or []
            failing_jobs = []
            for j in jobs:
                ex = j.get("exit")
                st = str(j.get("state") or "")
                loaded = j.get("loaded", True)
                if not loaded or (ex not in _BAD_EXIT_VALUES and not (st == "running" and str(ex) in _TERM_EXIT_VALUES)):
                    failing_jobs.append({
                        "label": j.get("label"),
                        "loaded": bool(loaded),
                        "exit": ex,
                        "when": j.get("when") or "",
                        "state": st,
                    })

            load = latest.get("load")
            load_1m = round(float(load[0]), 2) if isinstance(load, list) and load else None
            spark = [
                {
                    "t": s.get("t"),
                    "chip_c": s.get("chipC"),
                    "cpu": s.get("cpu"),
                    "disk_free_gb": s.get("diskFreeGB"),
                }
                for s in samples[-24:]
                if isinstance(s, dict)
            ]
            drives = [
                {
                    "name": d.get("name"),
                    "mounted": bool(d.get("mounted")),
                    "free_gb": _gb(d.get("free_bytes")),
                    "total_gb": _gb(d.get("total_bytes")),
                    "trend_24h_gb": _gb(d.get("trend_h24")),
                    "alert": d.get("alert"),
                }
                for d in disks
                if d.get("name") not in ("Recovery", "Preboot", "Update", "VM")
            ]
            mail_items = r.get("mail") or []
            unread = sum(1 for m in mail_items if isinstance(m, dict) and not m.get("read"))
            hosts_out.append({
                "id": hid,
                "collected_at": doc.get("collected_at"),
                "ok": bool(r.get("ok", True)),
                "owner_active": bool(r.get("owner_active")),
                "chip_c": latest.get("chipC"),
                "thermal": latest.get("thermal") or "unknown",
                "cpu_pct": latest.get("cpu"),
                "mem_pressure": latest.get("memPressure"),
                "mem_used_gb": latest.get("memUsedGB"),
                "mem_total_gb": latest.get("memTotalGB"),
                "load_1m": load_1m,
                "batt_pct": latest.get("batt"),
                "ac": latest.get("ac"),
                "disk_free_gb": free_gb,
                "disk_total_gb": total_gb,
                "disk_target_gb": tinfo.get("target_gb"),
                "disk_floor_gb": tinfo.get("floor_gb"),
                "syncthing": r.get("syncthing") or {},
                "glancebar": r.get("glancebar") or {},
                "free_space_guard": r.get("free_space_guard") or {},
                "unread_mail": unread,
                "failing_jobs_count": len(failing_jobs),
                "failing_jobs": failing_jobs,
                "fail_count": sum(1 for row in host_rows if row.get("status") == "fail"),
                "warn_count": sum(1 for row in host_rows if row.get("status") == "warn"),
                "drives": drives,
                "samples": spark,
            })
        return {"generated_at": int(time.time()), "hosts": hosts_out}

    def build_capacity(self) -> dict[str, Any]:
        paused = set(self._paused_providers())
        shared = _read_json(self.home / ".local/state/ai-capacity/shared.json") or {}
        posture = _read_json(self.home / ".local/state/ai-budget/posture.json") or {}
        pools_out: list[dict[str, Any]] = []
        services = shared.get("services")
        if isinstance(services, dict):
            for engine, svc in services.items():
                if not isinstance(svc, dict):
                    continue
                unavail = svc.get("unavailable")
                for p in svc.get("pools") or []:
                    if not isinstance(p, dict):
                        continue
                    est = p.get("estimate") if isinstance(p.get("estimate"), dict) else {}
                    pools_out.append({
                        "engine": engine,
                        "pool": p.get("pool") or "",
                        "remaining_pct": p.get("remainingPct", p.get("remaining_pct")),
                        "jobs_left": p.get("jobsLeft", est.get("jobsLeft")),
                        "tokens_left": p.get("tokensLeft", est.get("tokensLeft")),
                        "reset_at": p.get("resetAt"),
                        "stale": p.get("freshness") in ("stale", "unknown", "none"),
                        "freshness": p.get("freshness") or "unknown",
                        "paused": engine.lower() in paused,
                        "unavailable": unavail,
                    })
        elif isinstance(posture.get("pools"), dict):
            for key, p in posture["pools"].items():
                if not isinstance(p, dict):
                    continue
                engine, _, pname = key.partition(":")
                pools_out.append({
                    "engine": engine,
                    "pool": pname or key,
                    "remaining_pct": p.get("pct"),
                    "jobs_left": None,
                    "tokens_left": None,
                    "reset_at": p.get("resetsAt"),
                    "stale": bool(p.get("stale", True)),
                    "freshness": "stale" if p.get("stale") else "fresh",
                    "paused": engine.lower() in paused,
                    "unavailable": None,
                })
        return {
            "generated_at": shared.get("generatedAt") or posture.get("asOf") or _utc_iso(),
            "route": posture.get("route") or {},
            "paused": sorted(paused),
            "pools": pools_out,
        }

    def build_backups(self) -> dict[str, Any]:
        snaps = self._host_snapshots()
        idrive = _read_json(self.home / ".local/state/idrive-ingest/status.json") or {}
        receipts = _read_json(self.home / ".local/state/gdrive-backup/receipts.json") or {}

        home_mirror: dict[str, Any] = {}
        placement: dict[str, Any] = {}
        for hid, doc in snaps.items():
            r = doc.get("reading") or {}
            if isinstance(r.get("home_mirror"), dict):
                home_mirror[hid] = r["home_mirror"]
            if isinstance(r.get("placement"), dict) and r["placement"]:
                placement = r["placement"]
        if not placement:
            local_pl = _read_json(self.home / ".local/state/placement/status.json")
            if isinstance(local_pl, dict):
                placement = local_pl

        g_items = []
        active_runs = 0
        ok_count = 0
        for name, info in sorted(receipts.items()):
            if not isinstance(info, dict):
                continue
            if info.get("running"):
                active_runs += 1
            if info.get("last_ok") or info.get("ok"):
                ok_count += 1
            g_items.append({
                "name": name,
                "running": bool(info.get("running")),
                "last_attempt": info.get("last_attempt"),
                "last_ok": info.get("last_ok"),
                "last_result": info.get("last_result"),
            })

        prog = idrive.get("progress") if isinstance(idrive.get("progress"), dict) else {}
        last_ok = idrive.get("last_success") if isinstance(idrive.get("last_success"), dict) else {}
        return {
            "idrive": {
                "checked_at": idrive.get("checked_at"),
                "host_vm": idrive.get("host_vm"),
                "container": bool(idrive.get("container")),
                "running": bool(idrive.get("running")),
                "appending": bool(idrive.get("appending")),
                "percent": prog.get("percent"),
                "sent_bytes": prog.get("sent_bytes"),
                "total_bytes": prog.get("total_bytes"),
                "phase": (idrive.get("last") or {}).get("status"),
                "last_success_epoch": idrive.get("last_success_epoch"),
                "last_success_status": last_ok.get("status"),
                "storage": idrive.get("storage") or {},
            },
            "gdrive": {
                "total": len(g_items),
                "active_runs": active_runs,
                "ok_count": ok_count,
                "items": g_items[:50],
            },
            "home_mirror": home_mirror,
            "placement": {
                "generated": placement.get("generated"),
                "ok": placement.get("ok"),
                "violations_count": len(placement.get("violations") or []),
                "violations": (placement.get("violations") or [])[:50],
            },
        }

    def build_security(self) -> dict[str, Any]:
        check = _read_json(self.evidence_dir / "check.json") or {}
        status = _read_json(self.evidence_dir / "status.json") or {}
        audit_latest = _read_json(self.evidence_dir / "audit/latest.json") or {}

        ns_audits: dict[str, Any] = {}
        audit_root = self.evidence_dir / "audit"
        if audit_root.is_dir():
            for child in sorted(audit_root.iterdir()):
                if not child.is_dir():
                    continue
                files = sorted(child.glob("20*.json"), reverse=True)
                if not files:
                    continue
                doc = _read_json(files[0])
                if isinstance(doc, dict):
                    findings = doc.get("findings") or []
                    ns_audits[child.name] = {
                        "file": files[0].name,
                        "status": doc.get("status"),
                        "counts": doc.get("counts") or {},
                        "fail_count": sum(1 for f in findings if f.get("level") == "fail"),
                        "warn_count": sum(1 for f in findings if f.get("level") == "warn"),
                        "top_findings": findings[:30],
                    }

        tripwire_open = 0
        tripwire_dir = self.home / ".local/state/fleet/tripwire/incidents"
        if tripwire_dir.is_dir():
            for d in tripwire_dir.iterdir():
                if d.is_dir() and not (d / "report.md").exists():
                    tripwire_open += 1

        return {
            "status": check.get("status") or status.get("status") or "unknown",
            "value": check.get("value") or "",
            "datum": check.get("datum") or "",
            "detail": check.get("detail") or "",
            "generated": check.get("generated") or status.get("generated"),
            "rows": check.get("rows") or [],
            "namespaces": status.get("namespaces") or {},
            "audit_latest": audit_latest,
            "namespace_audits": ns_audits,
            "local_tripwire_open": tripwire_open,
        }

    def build_mail(self) -> dict[str, Any]:
        raw_inbox = _read_jsonl(self.home / ".local/state/agent-mail/inbox.jsonl", limit=60)
        raw_inbox.reverse()
        ack = self._load_mail_ack()
        ack_ids_set = set(ack["acked_ids"])

        inbox: list[dict[str, Any]] = []
        for entry in raw_inbox:
            msg = dict(entry)
            msg["read"] = bool(msg.get("read")) or self._is_msg_acked(msg.get("id"), msg.get("at"), ack, ack_ids_set)
            inbox.append(msg)

        snaps = self._host_snapshots()
        unread_by_host: dict[str, int] = {}
        for hid, doc in snaps.items():
            mail_list = (doc.get("reading") or {}).get("mail") or []
            unread_by_host[hid] = sum(
                1
                for m in mail_list
                if isinstance(m, dict)
                and not m.get("read")
                and not self._is_msg_acked(m.get("id"), m.get("at"), ack, ack_ids_set)
            )

        unread_total = (
            sum(unread_by_host.values())
            if unread_by_host
            else sum(1 for m in inbox if not m.get("read"))
        )

        board = _read_json(self.home / ".local/state/cowork/board.json") or {}
        claims_out = []
        for c in board.get("claims") or []:
            if not isinstance(c, dict):
                continue
            owner = c.get("owner") or {}
            claims_out.append({
                "work": c.get("work") or c.get("id") or "",
                "host": owner.get("host") or "",
                "provider": owner.get("provider") or "",
                "heartbeat": c.get("heartbeat"),
                "expires": c.get("expires"),
                "note": ((c.get("notes") or [{}])[-1]).get("text", ""),
            })
        return {
            "messages": inbox[:40],
            "unread_by_host": unread_by_host,
            "unread_total": unread_total,
            "acked_at": ack.get("acked_at"),
            "claims": claims_out,
        }

    def build_morning(self) -> dict[str, Any]:
        summary = self._morning_summary()
        rows, counts, snoozed_keys = self._annotate_rows(
            summary.get("rows") or [],
            summary.get("counts"),
        )
        return {
            "version": summary.get("version", 1),
            "generated": summary.get("generated"),
            "date": summary.get("date"),
            "ok": counts["fail"] == 0,
            "counts": counts,
            "cadence": summary.get("cadence") or {},
            "agy_morning": self._agy_morning_text(),
            "snoozed_count": len(snoozed_keys),
            "snoozed_keys": snoozed_keys,
            "rows": rows,
        }

    def build_services(self) -> dict[str, Any]:
        registry = self.mac_sync / "registry"
        jobs: dict[str, dict] = {}
        observations: dict[str, str] = {}
        catalog = registry / "jobs.tsv"

        def service(label: str) -> dict:
            return jobs.setdefault(label, {"label": label, "project": "", "purpose": "",
                                           "expected": "", "review": "", "observed": {}})

        try:
            lines = catalog.read_text(encoding="utf-8").splitlines()
            catalog_available = True
        except (OSError, UnicodeError):
            lines = []
            catalog_available = False
        for line in lines:
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            label, expected, purpose, project, review = (line.split("\t") + [""] * 4)[:5]
            if label.strip():
                service(label.strip()).update(project=project, purpose=purpose, expected=expected, review=review)

        for path in sorted((registry / "hosts").glob("*.json")):
            doc = _read_json(path, {})
            if (not HOST_ID_RE.fullmatch(path.stem) or not isinstance(doc, dict)
                    or doc.get("host") != path.stem or not isinstance(doc.get("jobs"), dict)):
                continue
            observations[path.stem] = str(doc.get("at") or "")
            for label, record in doc["jobs"].items():
                if not isinstance(record, dict):
                    continue
                if record.get("loaded") is False:
                    state = "unloaded"
                elif record.get("loaded") is not True:
                    state = "unknown"
                elif str(record.get("pid", "")).isdigit() and int(record["pid"]) > 0:
                    state = "running"
                elif record.get("status") in ("0", 0, "", "-"):
                    state = "idle"
                elif str(record.get("status", "")).lstrip("-").isdigit():
                    state = f"exit {record['status']}"
                else:
                    state = "unknown"
                service(label)["observed"][path.stem] = {"state": state, "schedule": str(record.get("when") or "")}
        owners = {row["project"] for row in jobs.values() if row["project"]}
        projects = {}
        for line in _read_text(registry / "descriptions.tsv").splitlines():
            fields = line.split("\t", 3)
            if len(fields) == 4 and fields[0] in owners:
                projects[fields[0]] = fields[3]
        return {"catalogAvailable": catalog_available, "projects": projects, "observations": observations,
                "services": [jobs[label] for label in sorted(jobs)]}

    def build_overview(self) -> dict[str, Any]:
        morning = self._morning_summary()
        hosts_doc = self.build_hosts()
        capacity = self.build_capacity()
        backups = self.build_backups()
        sec = self.build_security()
        mail = self.build_mail()
        total_unread = int(mail.get("unread_total") or 0)
        _, counts, snoozed_keys = self._annotate_rows(
            morning.get("rows") or [],
            morning.get("counts"),
        )
        sec_rows, _, _ = self._annotate_rows(sec.get("rows") or [], None)
        return {
            "generated_at": morning.get("generated") or int(time.time()),
            "date": morning.get("date") or _utc_iso()[:10],
            "counts": counts,
            "cadence": morning.get("cadence") or {},
            "agy_morning": self._agy_morning_text(),
            "action_hosts": self._action_hosts(),
            "snoozed_count": len(snoozed_keys),
            "snoozed_keys": snoozed_keys,
            "hosts": hosts_doc["hosts"],
            "capacity": capacity,
            "backups": {
                "idrive": backups["idrive"],
                "gdrive": {
                    "total": backups["gdrive"]["total"],
                    "active_runs": backups["gdrive"]["active_runs"],
                    "ok_count": backups["gdrive"]["ok_count"],
                },
                "home_mirror": backups["home_mirror"],
                "placement_violations": backups["placement"]["violations_count"],
            },
            "security": {
                "status": sec["status"],
                "value": sec["value"],
                "detail": sec["detail"],
                "generated": sec["generated"],
                "rows": sec_rows,
            },
            "unread_mail": total_unread,
            "active_claims": len(mail["claims"]),
        }

    def _audit(self, action: str, meta: dict[str, Any]) -> None:
        self.audit_log.parent.mkdir(parents=True, exist_ok=True)
        row = {"t": _utc_iso(), "action": action, **meta}
        with self.audit_log.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(row, separators=(",", ":")) + "\n")

    def action_mail(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        to = str(payload.get("to") or "").strip()
        text = str(payload.get("text") or "").strip()
        if not MAIL_TARGET_RE.match(to):
            return 400, {"ok": False, "error": "invalid target format"}
        if not text or len(text) > MAX_TEXT_LEN:
            return 400, {"ok": False, "error": "text must be 1..2000 chars"}
        tool = _expand(self.cfg["agent_mail_bin"])
        if not tool.is_file():
            found = shutil.which("agent-mail")
            if not found:
                return 500, {"ok": False, "error": "agent-mail not installed"}
            tool = Path(found)
        res = subprocess.run([str(tool), "send", to, text], capture_output=True, text=True, timeout=15)
        if res.returncode != 0:
            return 502, {"ok": False, "error": res.stderr.strip()[:200] or f"agent-mail exit {res.returncode}"}
        self._audit("mail", {"to": to, "len": len(text)})
        return 200, {"ok": True, "to": to, "id": res.stdout.strip()}

    def action_voice_note(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        text = str(payload.get("text") or "").strip()
        to = str(payload.get("to") or "").strip()
        if not text or len(text) > MAX_TEXT_LEN:
            return 400, {"ok": False, "error": "text must be 1..2000 chars"}
        if to and not MAIL_TARGET_RE.match(to):
            return 400, {"ok": False, "error": "invalid target format"}
        inbox_md = self.mac_sync / "VOICE-INBOX.md"
        inbox_md.parent.mkdir(parents=True, exist_ok=True)
        stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M:%SZ")
        with inbox_md.open("a", encoding="utf-8") as fh:
            fh.write(f"- [{stamp}] {text}\n")
        mail_id = None
        if to:
            code, resp = self.action_mail({"to": to, "text": f"[iPhone voice] {text}"})
            if code == 200:
                mail_id = resp.get("id")
        self._audit("voice-note", {"to": to or None, "len": len(text)})
        return 200, {"ok": True, "recorded": True, "mail_id": mail_id}

    def action_morning_refresh(self) -> tuple[int, dict[str, Any]]:
        with self._refresh_lock:
            if self._refresh_running:
                return 409, {"ok": False, "error": "morning refresh already running"}
            self._refresh_running = True

        fleet_bin = str(self.cfg["fleet_bin"])

        def worker() -> None:
            try:
                subprocess.run([fleet_bin, "morning"], capture_output=True, text=True, timeout=300)
            except Exception:
                pass
            finally:
                with self._refresh_lock:
                    self._refresh_running = False

        threading.Thread(target=worker, daemon=True).start()
        self._audit("morning-refresh", {"started": True})
        return 202, {"ok": True, "started": True}

    def action_provider_pause(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        provider = str(payload.get("provider") or "").strip().lower()
        paused_val = payload.get("paused")
        if provider not in ALLOWED_PAUSE_PROVIDERS or not isinstance(paused_val, bool):
            return 400, {"ok": False, "error": "provider must be claude|codex|zai|cursor and paused boolean"}
        path = self.home / ".config/agentkit/paused-providers"
        path.parent.mkdir(parents=True, exist_ok=True)
        lines = _read_text(path).splitlines()
        out_lines = []
        seen = False
        for line in lines:
            tok = line.split("#", 1)[0].strip().lower()
            if tok == provider:
                if paused_val and not seen:
                    out_lines.append(line)
                    seen = True
                continue
            out_lines.append(line)
        if paused_val and not seen:
            out_lines.append(provider)
        tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
        tmp.write_text("\n".join(out_lines) + ("\n" if out_lines else ""), encoding="utf-8")
        os.replace(tmp, path)
        if not paused_val:
            mark_path = self.home / ".local/state/ai-pick" / f"unavailable-{provider}.json"
            if mark_path.exists():
                mark_path.replace(mark_path.with_name(f"cleared-{provider}-{int(time.time())}.json"))
        self._audit("provider-pause", {"provider": provider, "paused": paused_val})
        return 200, {"ok": True, "provider": provider, "paused": paused_val, "list": self._paused_providers()}

    def action_mail_ack(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        raw_ids = payload.get("ids")
        if raw_ids is not None:
            if not isinstance(raw_ids, list) or not raw_ids or len(raw_ids) > 500:
                return 400, {"ok": False, "error": "ids must be a non-empty list (max 500)"}
            clean_ids: list[str] = []
            for item in raw_ids:
                if not isinstance(item, str) or not item.strip() or len(item.strip()) > 128:
                    return 400, {"ok": False, "error": "each id must be a non-empty string (<=128 chars)"}
                clean_ids.append(item.strip())
            with self._mail_ack_lock:
                ack = self._load_mail_ack()
                ack["acked_ids"].extend(clean_ids)
                self._save_mail_ack(ack)
            mail = self.build_mail()
            self._audit("mail-ack", {"mode": "ids", "count": len(clean_ids)})
            return 200, {
                "ok": True,
                "acked_ids_count": len(clean_ids),
                "acked_at": ack.get("acked_at"),
                "unread_total": mail["unread_total"],
            }

        raw_inbox = _read_jsonl(self.home / ".local/state/agent-mail/inbox.jsonl", limit=200)
        snaps = self._host_snapshots()
        seen_ids: list[str] = []
        max_epoch = time.time()
        for msg in raw_inbox:
            mid = msg.get("id")
            if isinstance(mid, str) and mid.strip():
                seen_ids.append(mid.strip())
            ep = _parse_mail_epoch(msg.get("at"))
            if ep is not None and ep > max_epoch:
                max_epoch = ep
        for doc in snaps.values():
            for item in (doc.get("reading") or {}).get("mail") or []:
                if not isinstance(item, dict):
                    continue
                mid = item.get("id")
                if isinstance(mid, str) and mid.strip():
                    seen_ids.append(mid.strip())
                ep = _parse_mail_epoch(item.get("at"))
                if ep is not None and ep > max_epoch:
                    max_epoch = ep

        acked_at = _utc_iso()
        with self._mail_ack_lock:
            ack = self._load_mail_ack()
            ack["acked_epoch"] = max(float(ack.get("acked_epoch") or 0.0), max_epoch)
            ack["acked_at"] = acked_at
            ack["acked_ids"].extend(seen_ids)
            self._save_mail_ack(ack)

        raw_tool = str(self.cfg.get("agent_mail_bin") or "").strip()
        if raw_tool:
            tool = _expand(raw_tool)
            if not tool.is_file() and raw_tool == DEFAULT_CONFIG["agent_mail_bin"]:
                found = shutil.which("agent-mail")
                tool = Path(found) if found else tool
            if tool.is_file():
                try:
                    subprocess.run([str(tool), "unread", "--mark", "--provider", "agy"], capture_output=True, text=True, timeout=10)
                except Exception:
                    pass

        mail = self.build_mail()
        self._audit("mail-ack", {"mode": "all", "ids_seen": len(seen_ids)})
        return 200, {
            "ok": True,
            "acked_at": acked_at,
            "unread_total": mail["unread_total"],
        }

    def action_snooze(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        key = str(payload.get("key") or "").strip()
        if not key or len(key) > 200 or "\n" in key or "\r" in key:
            return 400, {"ok": False, "error": "invalid row key"}
        if payload.get("clear") is True:
            with self._snooze_lock:
                items = self._load_snoozes()
                items.pop(key, None)
                self._save_snoozes(items)
            self._audit("unsnooze", {"key": key})
            return 200, {"ok": True, "key": key, "until": None, "detail": "Cleared snooze"}

        raw_hours = payload.get("hours", 24)
        if isinstance(raw_hours, bool) or not isinstance(raw_hours, (int, float)):
            return 400, {"ok": False, "error": "hours must be 1..720"}
        hours = int(raw_hours)
        if hours < 1 or hours > 720:
            return 400, {"ok": False, "error": "hours must be 1..720"}
        note = str(payload.get("note") or "").strip()[:200]
        until_epoch = time.time() + hours * 3600
        until_iso = _utc_iso(until_epoch)
        with self._snooze_lock:
            items = self._load_snoozes()
            items[key] = {"until_epoch": until_epoch, "until": until_iso, "note": note}
            self._save_snoozes(items)
        self._audit("snooze", {"key": key, "hours": hours})
        return 200, {"ok": True, "key": key, "until": until_iso, "detail": f"Snoozed for {hours}h"}

    def read_job_log(self, host: str, label: str, lines: int = 60) -> tuple[int, dict[str, Any]]:
        clean_host = host.strip().lower()
        clean_label = label.strip()
        if not HOST_ID_RE.match(clean_host):
            return 400, {"ok": False, "error": "invalid host"}
        if clean_host not in self._action_hosts():
            return 403, {"ok": False, "error": f"actions not permitted on host {clean_host!r}"}
        if not JOB_LABEL_RE.match(clean_label) or ".." in clean_label or "/" in clean_label:
            return 400, {"ok": False, "error": "invalid job label"}
        if not self._is_allowed_job_label(clean_label):
            return 403, {"ok": False, "error": f"label {clean_label!r} not in allowed prefixes"}
        return self._run_host_job_script(clean_host, "job-log", clean_label, str(max(10, min(100, lines))))

    def action_job_kickstart(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        clean_host = str(payload.get("host") or "").strip().lower()
        clean_label = str(payload.get("label") or "").strip()
        if not HOST_ID_RE.match(clean_host):
            return 400, {"ok": False, "error": "invalid host"}
        if clean_host not in self._action_hosts():
            return 403, {"ok": False, "error": f"actions not permitted on host {clean_host!r}"}
        if not JOB_LABEL_RE.match(clean_label) or ".." in clean_label or "/" in clean_label:
            return 400, {"ok": False, "error": "invalid job label"}
        if not self._is_allowed_job_label(clean_label):
            return 403, {"ok": False, "error": f"label {clean_label!r} not in allowed prefixes"}
        code, body = self._run_host_job_script(clean_host, "job-kickstart", clean_label)
        self._audit("job-kickstart", {"host": clean_host, "label": clean_label, "ok": body.get("ok"), "state": body.get("state")})
        return code, body

    def action_host_action(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        clean_host = str(payload.get("host") or "").strip().lower()
        kind = str(payload.get("kind") or "").strip().lower()
        if not HOST_ID_RE.match(clean_host):
            return 400, {"ok": False, "error": "invalid host"}
        if clean_host not in self._action_hosts():
            return 403, {"ok": False, "error": f"actions not permitted on host {clean_host!r}"}
        suffix = HOST_ACTION_SUFFIXES.get(kind)
        if not suffix:
            return 400, {"ok": False, "error": "unknown host action kind"}
        code, body = self._run_host_job_script(clean_host, "host-action", suffix)
        self._audit("host-action", {"host": clean_host, "kind": kind, "ok": body.get("ok"), "label": body.get("label")})
        return code, body

    def action_dispatch_fix(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        to = str(payload.get("to") or "").strip()
        prompt = str(payload.get("prompt") or "").strip()
        key = str(payload.get("key") or "").strip()[:120]
        title = str(payload.get("title") or "").strip()[:120]
        if not MAIL_TARGET_RE.match(to):
            return 400, {"ok": False, "error": "invalid target format"}
        if not prompt or len(prompt) > 1800:
            return 400, {"ok": False, "error": "prompt must be 1..1800 chars"}
        prefix = f"[Fleet iPhone · {title or key}] " if (title or key) else "[Fleet iPhone] "
        full_text = (prefix + prompt)[:MAX_TEXT_LEN]
        ok, out, code = self._send_agent_mail(to, full_text)
        if not ok:
            return code, {"ok": False, "error": out}
        self._audit("dispatch-fix", {"to": to, "key": key or None, "len": len(full_text)})
        return 200, {"ok": True, "to": to, "id": out, "detail": f"Dispatched to {to}"}

    # MARK: - Drive Brief & Override Plan

    _VIRTUAL_LOCATION_TOKENS = frozenset({
        "online", "virtual", "remote", "phone", "call", "tbd",
        "zoom", "teams", "google meet", "slack huddle", "none", "n/a", "na", "-",
    })
    _VIRTUAL_URL_HOSTS = (
        "http://", "https://", "zoom.us", "teams.microsoft",
        "meet.google", "webex", "facetime", "whereby", "gotomeeting",
    )
    _DEFAULT_DRIVE_STATIONS: tuple[dict[str, str], ...] = (
        {"id": "kexp", "name": "KEXP"},
        {"id": "doublej", "name": "Double J"},
        {"id": "rrr", "name": "RRR"},
        {"id": "pbs", "name": "PBS"},
        {"id": "bbc6", "name": "6 Music"},
        {"id": "nts", "name": "NTS"},
        {"id": "rp", "name": "Radio Paradise"},
    )
    _DAY_ABBR = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")

    @classmethod
    def _is_physical_location(cls, raw: str) -> bool:
        cleaned = raw.strip()
        if not cleaned or len(cleaned) < 2:
            return False
        folded = " ".join(cleaned.lower().split())
        if folded in cls._VIRTUAL_LOCATION_TOKENS:
            return False
        if any(tok in folded for tok in cls._VIRTUAL_URL_HOSTS):
            return False
        return True

    @staticmethod
    def _haversine_m(lat1: float, lon1: float, lat2: float, lon2: float) -> float:
        r_m = 6_371_000.0
        phi1 = math.radians(lat1)
        phi2 = math.radians(lat2)
        d_phi = math.radians(lat2 - lat1)
        d_lam = math.radians(lon2 - lon1)
        a = math.sin(d_phi / 2.0) ** 2 + math.cos(phi1) * math.cos(phi2) * math.sin(d_lam / 2.0) ** 2
        return 2.0 * r_m * math.atan2(math.sqrt(a), math.sqrt(max(0.0, 1.0 - a)))

    @staticmethod
    def _build_waze_url(dest: dict[str, Any] | None) -> str:
        if not isinstance(dest, dict):
            return "waze://"
        lat = dest.get("lat")
        lon = dest.get("lon")
        if isinstance(lat, (int, float)) and not isinstance(lat, bool) and isinstance(lon, (int, float)) and not isinstance(lon, bool):
            return f"waze://?ll={float(lat):.6f},{float(lon):.6f}&navigate=yes"
        query = str(dest.get("query") or "").strip()
        if query:
            return f"waze://?q={urllib.parse.quote(query, safe='')}&navigate=yes"
        return "waze://"

    @staticmethod
    def _optional_coord(val: Any) -> float | None:
        if isinstance(val, bool) or val is None or val == "":
            return None
        try:
            num = float(val)
        except (TypeError, ValueError):
            return None
        return num if math.isfinite(num) else None

    @staticmethod
    def _clamp_volume(raw_vol: Any, default: float = 0.45) -> float:
        try:
            if isinstance(raw_vol, bool) or raw_vol is None:
                val = default
            else:
                val = float(raw_vol)
        except (TypeError, ValueError):
            val = default
        if not math.isfinite(val):
            val = default
        return round(min(0.85, max(0.15, val)), 2)

    def _load_drive_stations(self, places_doc: dict[str, Any] | None = None) -> list[dict[str, str]]:
        def _parse(raw_list: Any) -> list[dict[str, str]]:
            out: list[dict[str, str]] = []
            seen: set[str] = set()
            if isinstance(raw_list, list):
                for item in raw_list:
                    if not isinstance(item, dict):
                        continue
                    sid = str(item.get("id") or "").strip()
                    name = str(item.get("name") or sid).strip()
                    if sid and sid not in seen:
                        seen.add(sid)
                        out.append({"id": sid, "name": name})
            return out

        custom = _parse(places_doc.get("stations") if isinstance(places_doc, dict) else None)
        if custom:
            return custom
        doc = _read_json(self.music_stations_file, None)
        raw_list = doc.get("stations") if isinstance(doc, dict) else (doc if isinstance(doc, list) else None)
        loaded = _parse(raw_list)
        return loaded if loaded else [dict(s) for s in self._DEFAULT_DRIVE_STATIONS]

    def _load_drive_places(self) -> dict[str, Any]:
        doc = _read_json(self.drive_places_file, {})
        return doc if isinstance(doc, dict) else {}

    def _load_active_drive_plan(self) -> dict[str, Any] | None:
        with self._drive_lock:
            doc = _read_json(self.drive_plan_file, None)
        if not isinstance(doc, dict):
            return None
        exp = self._optional_coord(doc.get("expires_at"))
        if exp is not None and exp <= time.time():
            return None
        return doc

    def _save_drive_plan(self, plan: dict[str, Any]) -> None:
        with self._drive_lock:
            self.drive_plan_file.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.drive_plan_file.with_name(f".{self.drive_plan_file.name}.{os.getpid()}.tmp")
            tmp.write_text(json.dumps(plan, separators=(",", ":")), encoding="utf-8")
            os.chmod(tmp, 0o600)
            os.replace(tmp, self.drive_plan_file)

    def _clear_drive_plan(self) -> None:
        with self._drive_lock:
            try:
                self.drive_plan_file.unlink()
            except FileNotFoundError:
                pass

    @staticmethod
    def _place_to_dest(place_id: str, place: dict[str, Any], source: str) -> dict[str, Any] | None:
        if not isinstance(place, dict):
            return None
        name = str(place.get("name") or place_id).strip() or place_id
        query = str(place.get("query") or place.get("address") or name).strip()
        lat = FleetDataStore._optional_coord(place.get("lat"))
        lon = FleetDataStore._optional_coord(place.get("lon"))
        if lat is None or lon is None:
            lat, lon = None, None
        if not query and lat is None:
            return None
        return {
            "name": name,
            "query": query or name,
            "lat": lat,
            "lon": lon,
            "source": source,
            "place_id": place_id,
            "event_start": None,
        }

    def _is_already_at_place(self, lat: float | None, lon: float | None, place: dict[str, Any]) -> bool:
        if lat is None or lon is None or not isinstance(place, dict):
            return False
        p_lat = self._optional_coord(place.get("lat"))
        p_lon = self._optional_coord(place.get("lon"))
        if p_lat is None or p_lon is None:
            return False
        radius = self._optional_coord(place.get("radius_m")) or 400.0
        return self._haversine_m(lat, lon, p_lat, p_lon) <= max(50.0, radius)

    def build_drive_brief(
        self,
        lat: float | None = None,
        lon: float | None = None,
        event_title: str = "",
        event_location: str = "",
        event_start: str = "",
        hour: int | None = None,
        dow: int | None = None,
        shuffle: bool = False,
    ) -> dict[str, Any]:
        places_doc = self._load_drive_places()
        plan = self._load_active_drive_plan()
        stations = self._load_drive_stations(places_doc)
        stations_by_id = {s["id"]: s for s in stations}

        now_local = dt.datetime.now()
        cur_hour = now_local.hour if hour is None else max(0, min(23, int(hour)))
        cur_dow = now_local.weekday() if dow is None else (int(dow) % 7)
        dow_abbr = self._DAY_ABBR[cur_dow]

        raw_places = places_doc.get("places")
        places: dict[str, dict[str, Any]] = raw_places if isinstance(raw_places, dict) else {}

        dest: dict[str, Any] | None = None

        # 1. Active override plan
        if plan:
            p_pid = str(plan.get("place_id") or "").strip()
            p_query = str(plan.get("destination_query") or "").strip()
            p_name = str(plan.get("destination_name") or "").strip()
            p_lat = self._optional_coord(plan.get("lat"))
            p_lon = self._optional_coord(plan.get("lon"))
            if p_pid and p_pid in places:
                dest = self._place_to_dest(p_pid, places[p_pid], "override")
                if dest and p_name:
                    dest["name"] = p_name
            elif p_query or (p_lat is not None and p_lon is not None):
                dest = {
                    "name": p_name or p_query or "Pinned Destination",
                    "query": p_query or p_name or f"{p_lat:.6f},{p_lon:.6f}",
                    "lat": p_lat if (p_lat is not None and p_lon is not None) else None,
                    "lon": p_lon if (p_lat is not None and p_lon is not None) else None,
                    "source": "override",
                    "place_id": None,
                    "event_start": None,
                }

        # 2. Upcoming Calendar event with a physical location
        if dest is None and self._is_physical_location(event_location):
            ev_loc = event_location.strip()
            ev_title = event_title.strip()
            dest = {
                "name": ev_title or ev_loc,
                "query": ev_loc,
                "lat": None,
                "lon": None,
                "source": "calendar",
                "place_id": None,
                "event_start": event_start.strip() or None,
            }

        # 3. Configured routines (days + hour window + optional when_near)
        if dest is None:
            raw_routines = places_doc.get("routines")
            if isinstance(raw_routines, list):
                for routine in raw_routines:
                    if not isinstance(routine, dict):
                        continue
                    days = [str(d).strip().lower() for d in (routine.get("days") or []) if str(d).strip()]
                    if days and dow_abbr not in days:
                        continue
                    start_h = routine.get("start_hour")
                    end_h = routine.get("end_hour")
                    if isinstance(start_h, int) and isinstance(end_h, int):
                        if start_h <= end_h:
                            if not (start_h <= cur_hour < end_h):
                                continue
                        else:
                            if not (cur_hour >= start_h or cur_hour < end_h):
                                continue
                    when_near = str(routine.get("when_near") or "").strip()
                    if when_near and lat is not None and lon is not None:
                        near_place = places.get(when_near)
                        if not near_place or not self._is_already_at_place(lat, lon, near_place):
                            continue
                    target_id = str(routine.get("place_id") or routine.get("place") or "").strip()
                    target_place = places.get(target_id) if target_id else None
                    if not target_place:
                        continue
                    if self._is_already_at_place(lat, lon, target_place):
                        continue
                    dest = self._place_to_dest(target_id, target_place, "routine")
                    if dest:
                        if routine.get("label"):
                            dest["name"] = str(routine["label"]).strip()
                        break

        # 4. Return home if away from home coordinates
        if dest is None and lat is not None and lon is not None and isinstance(places.get("home"), dict):
            home_place = places["home"]
            if not self._is_already_at_place(lat, lon, home_place):
                dest = self._place_to_dest("home", home_place, "return_home")

        waze_url = self._build_waze_url(dest)

        # Music selection
        if 5 <= cur_hour <= 10:
            bucket = "morning"
        elif 11 <= cur_hour <= 14:
            bucket = "midday"
        elif 15 <= cur_hour <= 17:
            bucket = "afternoon"
        elif 18 <= cur_hour <= 21:
            bucket = "evening"
        else:
            bucket = "night"

        music_schedule = places_doc.get("music_schedule")
        bucket_ids: list[str] = []
        if isinstance(music_schedule, dict) and isinstance(music_schedule.get(bucket), list):
            bucket_ids = [str(s).strip() for s in music_schedule[bucket] if str(s).strip()]

        matched_ids = [sid for sid in bucket_ids if sid in stations_by_id]
        pool_ids = matched_ids if matched_ids else ([s["id"] for s in stations] or bucket_ids)

        override_sid = str((plan or {}).get("station_id") or "").strip()
        override_mood = str((plan or {}).get("music_mood") or "").strip() or None
        plan_expires_at = self._optional_coord((plan or {}).get("expires_at"))

        if override_sid and not shuffle:
            chosen_id = override_sid
            chosen_name = stations_by_id.get(chosen_id, {}).get("name") or chosen_id.replace("-", " ").title()
            msource = "override"
        else:
            if not pool_ids:
                pool_ids = [s["id"] for s in self._DEFAULT_DRIVE_STATIONS]
            day_idx = now_local.timetuple().tm_yday
            with self._drive_lock:
                if shuffle:
                    self._drive_shuffle_idx += 1
                offset = self._drive_shuffle_idx
            idx = (day_idx + offset) % len(pool_ids)
            if shuffle and override_sid and len(pool_ids) > 1 and pool_ids[idx] == override_sid:
                idx = (idx + 1) % len(pool_ids)
            chosen_id = pool_ids[idx]
            chosen_name = stations_by_id.get(chosen_id, {}).get("name") or chosen_id.replace("-", " ").title()
            msource = "shuffle" if shuffle else f"schedule:{bucket}"

        lens_id = chosen_id if chosen_id.startswith("lens:") or chosen_id.startswith("home-station:") else f"home-station:{chosen_id}"
        music = {
            "station_id": chosen_id,
            "station_name": chosen_name,
            "lens_id": lens_id,
            "mood": override_mood or bucket,
            "source": msource,
            "url": f"ontotheradio://play?station={urllib.parse.quote(chosen_id, safe='')}",
        }

        default_vol = places_doc.get("default_volume")
        raw_vol = (plan or {}).get("volume") if (plan and plan.get("volume") is not None) else default_vol
        volume = self._clamp_volume(raw_vol, default=0.45)
        volume_pct = int(round(volume * 100))

        if dest:
            summary = f"Waze → {dest['name']} · {chosen_name} · {volume_pct}%"
        else:
            summary = f"Waze · {chosen_name} · {volume_pct}%"

        return {
            "ok": True,
            "generated_at": int(time.time()),
            "destination": dest,
            "waze_url": waze_url,
            "music": music,
            "volume": volume,
            "volume_pct": volume_pct,
            "summary": summary,
            "expires_at": int(plan_expires_at) if plan_expires_at is not None else None,
        }

    def action_drive_plan(self, payload: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        if payload.get("clear") is True:
            self._clear_drive_plan()
            self._audit("drive-plan-clear", {})
            return 200, {"ok": True, "cleared": True, "brief": self.build_drive_brief()}

        raw_ttl = payload.get("ttl_hours", 6)
        if isinstance(raw_ttl, bool) or not isinstance(raw_ttl, (int, float)):
            return 400, {"ok": False, "error": "ttl_hours must be 1..72"}
        ttl_hours = float(raw_ttl)
        if ttl_hours < 1 or ttl_hours > 72:
            return 400, {"ok": False, "error": "ttl_hours must be 1..72"}

        place_id = str(payload.get("place_id") or "").strip()[:80]
        dest_name = str(payload.get("destination_name") or "").strip()[:120]
        dest_query = str(payload.get("destination_query") or "").strip()[:300]
        station_id = str(payload.get("station_id") or "").strip()[:80]
        music_mood = str(payload.get("music_mood") or "").strip()[:60]
        note = str(payload.get("note") or "").strip()[:200]

        lat = self._optional_coord(payload.get("lat"))
        lon = self._optional_coord(payload.get("lon"))
        if (lat is None) != (lon is None):
            return 400, {"ok": False, "error": "lat and lon must both be provided"}

        vol_val: float | None = None
        if payload.get("volume") is not None:
            if isinstance(payload.get("volume"), bool) or not isinstance(payload.get("volume"), (int, float)):
                return 400, {"ok": False, "error": "volume must be numeric 0.15..0.85"}
            vol_val = self._clamp_volume(payload["volume"])
        elif payload.get("volume_pct") is not None:
            if isinstance(payload.get("volume_pct"), bool) or not isinstance(payload.get("volume_pct"), (int, float)):
                return 400, {"ok": False, "error": "volume_pct must be numeric 15..85"}
            vol_val = self._clamp_volume(float(payload["volume_pct"]) / 100.0)

        if not any([place_id, dest_query, lat is not None, station_id, music_mood, vol_val is not None]):
            return 400, {"ok": False, "error": "provide destination, station, mood, or volume"}

        expires_at = int(time.time() + ttl_hours * 3600)
        plan: dict[str, Any] = {
            "set_at": int(time.time()),
            "expires_at": expires_at,
            "place_id": place_id or None,
            "destination_name": dest_name or None,
            "destination_query": dest_query or None,
            "lat": lat,
            "lon": lon,
            "station_id": station_id or None,
            "music_mood": music_mood or None,
            "volume": vol_val,
            "note": note or None,
        }
        self._save_drive_plan(plan)
        self._audit("drive-plan-set", {
            "place_id": plan["place_id"],
            "has_query": bool(dest_query),
            "station_id": plan["station_id"],
            "ttl_hours": ttl_hours,
        })
        return 200, {"ok": True, "plan": plan, "brief": self.build_drive_brief()}


class FleetHTTPServer(ThreadingHTTPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, addr: tuple[str, int], store: FleetDataStore, expected_token: str, cfg: dict[str, Any]) -> None:
        self.store = store
        self.expected_token = expected_token
        self.cfg = cfg
        self.read_limiter = RateLimiter(int(cfg.get("read_rate_limit_per_min", 60)))
        self.write_limiter = RateLimiter(int(cfg.get("write_rate_limit_per_min", 20)))
        super().__init__(addr, FleetAPIHandler)


class FleetAPIHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "fleet-api"
    sys_version = ""

    @property
    def srv(self) -> FleetHTTPServer:
        return self.server  # type: ignore[return-value]

    def log_message(self, fmt: str, *args: Any) -> None:
        msg = fmt % args
        msg = re.sub(r"([?&]t=)[^&\s]+", r"\1REDACTED", msg)
        xff = self.headers.get("X-Forwarded-For", "").split(",")[0].strip() if self.headers else ""
        ip_raw = xff or (self.client_address[0] if self.client_address else "-")
        ip = re.sub(r"[^0-9a-fA-F.:]", "", ip_raw)[:45] or "-"
        ts_raw = self.headers.get("Tailscale-User-Login", "").strip() if self.headers else ""
        ts_user = re.sub(r"[^A-Za-z0-9._@+-]", "", ts_raw)[:80]
        ua_raw = self.headers.get("User-Agent", "-") if self.headers else "-"
        ua = re.sub(r'[\x00-\x1f"\s]+', " ", ua_raw).strip()[:80] or "-"
        user_part = f" user={ts_user}" if ts_user else ""
        sys.stderr.write(f'[{_utc_iso()}] ip={ip}{user_part} {msg} ua="{ua}"\n')

    def _send_bytes(self, status: int, content_type: str, data: bytes, head_only: bool = False) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        if not head_only:
            self.wfile.write(data)

    def _send_json(self, status: int, obj: Any, head_only: bool = False) -> None:
        body = json.dumps(obj, separators=(",", ":")).encode("utf-8")
        self._send_bytes(status, "application/json; charset=utf-8", body, head_only=head_only)

    def _client_key(self) -> str:
        fwd = self.headers.get("X-Forwarded-For", "").split(",")[0].strip()
        return fwd or (self.client_address[0] if self.client_address else "local")

    def _extract_token(self, qs: dict[str, list[str]], allow_query: bool) -> str:
        auth = self.headers.get("Authorization", "")
        if auth.startswith("Bearer "):
            return auth[7:].strip()
        hdr = self.headers.get("X-Fleet-Token", "").strip()
        if hdr:
            return hdr
        if allow_query and "t" in qs and qs["t"]:
            return qs["t"][0].strip()
        return ""

    def do_HEAD(self) -> None:
        self._handle_get(head_only=True)

    def do_GET(self) -> None:
        self._handle_get(head_only=False)

    def _handle_get(self, head_only: bool) -> None:
        parsed = urllib.parse.urlsplit(self.path)
        path = parsed.path
        qs = urllib.parse.parse_qs(parsed.query)
        is_ota = path.startswith("/v1/ota/")
        tok = self._extract_token(qs, allow_query=is_ota)
        if not verify_token(tok, self.srv.expected_token):
            if is_ota:
                self._send_bytes(HTTPStatus.NOT_FOUND, "text/plain", b"", head_only=True)
            else:
                self._send_json(HTTPStatus.UNAUTHORIZED, {"ok": False, "error": "unauthorized"}, head_only=head_only)
            return
        if not self.srv.read_limiter.allow(self._client_key()):
            self._send_json(HTTPStatus.TOO_MANY_REQUESTS, {"ok": False, "error": "rate limit exceeded"}, head_only=head_only)
            return

        if path == "/v1/health":
            self._send_json(HTTPStatus.OK, {
                "ok": True,
                "host": socket.gethostname().split(".")[0],
                "server_time": int(time.time()),
                "uptime_s": round(time.monotonic() - self.srv.store.started_at, 1),
                "api_version": API_VERSION,
            }, head_only=head_only)
            return

        routes: dict[str, Callable[[], dict[str, Any]]] = {
            "/v1/services": self.srv.store.build_services,
            "/v1/overview": self.srv.store.build_overview,
            "/v1/hosts": self.srv.store.build_hosts,
            "/v1/morning": self.srv.store.build_morning,
            "/v1/capacity": self.srv.store.build_capacity,
            "/v1/backups": self.srv.store.build_backups,
            "/v1/security": self.srv.store.build_security,
            "/v1/mail": self.srv.store.build_mail,
        }
        if path in routes:
            self._send_json(HTTPStatus.OK, routes[path](), head_only=head_only)
            return

        if path == "/v1/drive/brief":
            def _opt_str(key: str) -> str:
                return (qs.get(key) or [""])[0]

            def _opt_int(key: str) -> int | None:
                raw = _opt_str(key).strip()
                if not raw:
                    return None
                try:
                    return int(raw)
                except ValueError:
                    return None

            lat_val = FleetDataStore._optional_coord(_opt_str("lat"))
            lon_val = FleetDataStore._optional_coord(_opt_str("lon"))
            if (lat_val is None) != (lon_val is None):
                lat_val, lon_val = None, None
            shuffle_flag = _opt_str("shuffle").strip().lower() in ("1", "true", "yes")
            body = self.srv.store.build_drive_brief(
                lat=lat_val,
                lon=lon_val,
                event_title=_opt_str("event_title"),
                event_location=_opt_str("event_location"),
                event_start=_opt_str("event_start"),
                hour=_opt_int("hour"),
                dow=_opt_int("dow"),
                shuffle=shuffle_flag,
            )
            self._send_json(HTTPStatus.OK, body, head_only=head_only)
            return

        if path == "/v1/job-log":
            host_arg = (qs.get("host") or [""])[0]
            label_arg = (qs.get("label") or [""])[0]
            raw_lines = (qs.get("lines") or ["60"])[0]
            try:
                lines_n = int(raw_lines)
            except ValueError:
                lines_n = 60
            code, body = self.srv.store.read_job_log(host_arg, label_arg, lines=lines_n)
            self._send_json(code, body, head_only=head_only)
            return

        if is_ota:
            self._serve_ota(path, tok, head_only=head_only)
            return

        self._send_json(HTTPStatus.NOT_FOUND, {"ok": False, "error": "not found"}, head_only=head_only)

    def _serve_ota(self, path: str, tok: str, head_only: bool) -> None:
        # Installation and pairing links carry credentials. Their origin must be
        # an operator-selected HTTPS endpoint, independent of proxy headers.
        base = str(self.srv.cfg.get("public_base") or "").rstrip("/")
        if path in ("/v1/ota/install", "/v1/ota/manifest.plist"):
            try:
                origin = urllib.parse.urlsplit(base)
                _ = origin.port  # Reject malformed ports before generating links.
                configured = (origin.scheme == "https" and origin.hostname
                              and origin.hostname != "hub.example.ts.net"
                              and origin.username is None and origin.password is None
                              and not origin.query and not origin.fragment)
            except ValueError:
                configured = False
            if not configured:
                self._send_json(HTTPStatus.SERVICE_UNAVAILABLE, {"ok": False, "error": "configure HTTPS public_base for OTA"}, head_only=head_only)
                return
        qt = urllib.parse.quote(tok, safe="")
        title = str(self.srv.cfg.get("app_title") or "Fleet")
        bundle_id = str(self.srv.cfg.get("bundle_id") or "org.example.fleet")
        ipa_path = self.srv.store.ota_dir / "Fleet.ipa"

        if path == "/v1/ota/install":
            manifest_url = urllib.parse.quote(f"{base}/v1/ota/manifest.plist?t={qt}", safe="")
            itms_url = f"itms-services://?action=download-manifest&url={manifest_url}"
            pair_url = f"fleet://pair?url={urllib.parse.quote(base, safe='')}&token={qt}"
            ready_txt = f"IPA ready ({ipa_path.stat().st_size / 1e6:.1f} MB)" if ipa_path.is_file() else "IPA not uploaded yet"
            page = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{html.escape(title)} iOS</title>
<style>
body{{font-family:-apple-system,system-ui,sans-serif;background:#0a0d12;color:#f3f6fb;margin:0;padding:24px;display:flex;justify-content:center}}
.card{{max-width:440px;width:100%;background:#131922;border:1px solid #263142;border-radius:20px;padding:24px}}
h1{{margin:0 0 8px;font-size:26px}} p{{color:#9fb0c7;line-height:1.45}}
a.btn{{display:block;text-align:center;text-decoration:none;padding:15px 18px;border-radius:14px;font-weight:700;margin-top:14px}}
.primary{{background:#22c55e;color:#05140a}} .secondary{{background:#1e293b;color:#e2e8f0;border:1px solid #334155}}
.meta{{font-size:13px;color:#7a8ba3;margin-top:16px}}
</style></head><body><div class="card">
<h1>{html.escape(title)}</h1>
<p>Tailnet-only Mission Control for your Mac fleet.</p>
<a class="btn primary" href="{html.escape(itms_url)}">1. Install {html.escape(title)} IPA</a>
<a class="btn secondary" href="{html.escape(pair_url)}">2. Pair App with Hub</a>
<p class="meta">Bundle: {html.escape(bundle_id)} · {html.escape(ready_txt)}</p>
</div></body></html>"""
            self._send_bytes(HTTPStatus.OK, "text/html; charset=utf-8", page.encode("utf-8"), head_only=head_only)
            return

        if path == "/v1/ota/manifest.plist":
            ipa_url = f"{base}/v1/ota/Fleet.ipa?t={qt}"
            manifest = {
                "items": [{
                    "assets": [{"kind": "software-package", "url": ipa_url}],
                    "metadata": {
                        "bundle-identifier": bundle_id,
                        "bundle-version": "1.0.0",
                        "kind": "software",
                        "title": title,
                    },
                }]
            }
            data = plistlib.dumps(manifest, fmt=plistlib.FMT_XML)
            self._send_bytes(HTTPStatus.OK, "application/xml; charset=utf-8", data, head_only=head_only)
            return

        if path == "/v1/ota/Fleet.ipa":
            if not ipa_path.is_file():
                self._send_json(HTTPStatus.NOT_FOUND, {"ok": False, "error": "Fleet.ipa not published"}, head_only=head_only)
                return
            size = ipa_path.stat().st_size
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(size))
            self.send_header("Content-Disposition", 'attachment; filename="Fleet.ipa"')
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            if not head_only:
                with ipa_path.open("rb") as fh:
                    shutil.copyfileobj(fh, self.wfile, length=256 * 1024)
            return

        self._send_bytes(HTTPStatus.NOT_FOUND, "text/plain", b"", head_only=True)

    def do_POST(self) -> None:
        parsed = urllib.parse.urlsplit(self.path)
        path = parsed.path
        qs = urllib.parse.parse_qs(parsed.query)
        tok = self._extract_token(qs, allow_query=False)
        if not verify_token(tok, self.srv.expected_token):
            self._send_json(HTTPStatus.UNAUTHORIZED, {"ok": False, "error": "unauthorized"})
            return
        if not self.srv.write_limiter.allow(self._client_key()):
            self._send_json(HTTPStatus.TOO_MANY_REQUESTS, {"ok": False, "error": "rate limit exceeded"})
            return

        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            self.close_connection = True
            self._send_json(HTTPStatus.BAD_REQUEST, {"ok": False, "error": "invalid Content-Length"})
            return
        if length < 0 or length > MAX_BODY_BYTES:
            self.close_connection = True
            self._send_json(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, {"ok": False, "error": "body too large"})
            return
        raw = self.rfile.read(length) if length > 0 else b"{}"
        try:
            payload = json.loads(raw.decode("utf-8")) if raw.strip() else {}
        except (UnicodeDecodeError, ValueError):
            self._send_json(HTTPStatus.BAD_REQUEST, {"ok": False, "error": "invalid JSON"})
            return
        if not isinstance(payload, dict):
            self._send_json(HTTPStatus.BAD_REQUEST, {"ok": False, "error": "JSON object required"})
            return

        if path == "/v1/actions/mail":
            code, body = self.srv.store.action_mail(payload)
        elif path == "/v1/actions/voice-note":
            code, body = self.srv.store.action_voice_note(payload)
        elif path == "/v1/actions/morning-refresh":
            code, body = self.srv.store.action_morning_refresh()
        elif path == "/v1/actions/provider-pause":
            code, body = self.srv.store.action_provider_pause(payload)
        elif path == "/v1/actions/mail-ack":
            code, body = self.srv.store.action_mail_ack(payload)
        elif path == "/v1/actions/snooze":
            code, body = self.srv.store.action_snooze(payload)
        elif path == "/v1/actions/job-kickstart":
            code, body = self.srv.store.action_job_kickstart(payload)
        elif path == "/v1/actions/host-action":
            code, body = self.srv.store.action_host_action(payload)
        elif path == "/v1/actions/dispatch-fix":
            code, body = self.srv.store.action_dispatch_fix(payload)
        elif path == "/v1/actions/drive-plan":
            code, body = self.srv.store.action_drive_plan(payload)
        else:
            code, body = HTTPStatus.NOT_FOUND, {"ok": False, "error": "unknown action"}
        self._send_json(code, body)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Fleet tailnet JSON API & OTA server")
    sub = ap.add_subparsers(dest="cmd", required=True)
    srv = sub.add_parser("serve", help="Run the HTTP server")
    srv.add_argument("--config", type=Path, default=None)
    services = sub.add_parser("services", help="Find projects, placement and last observed launchd state")
    services.add_argument("query", nargs="?", default="")
    services.add_argument("--json", action="store_true")
    services.add_argument("--config", type=Path)

    drv = sub.add_parser("drive", help="Inspect or override the CarPlay drive brief")
    drv_sub = drv.add_subparsers(dest="drive_cmd", required=True)
    drv_brief = drv_sub.add_parser("brief", help="Show the computed drive brief")
    drv_brief.add_argument("--json", action="store_true")
    drv_brief.add_argument("--shuffle", action="store_true")
    drv_brief.add_argument("--config", type=Path)

    drv_set = drv_sub.add_parser("set", help="Pre-set destination, station, mood, or volume")
    drv_set.add_argument("--to", dest="query", default="", help="Destination address or search query")
    drv_set.add_argument("--name", default="", help="Display label for destination")
    drv_set.add_argument("--place", default="", help="Saved place ID from drive-places.json")
    drv_set.add_argument("--station", default="", help="Music station ID")
    drv_set.add_argument("--mood", default="", help="Music mood override")
    drv_set.add_argument("--volume", type=int, default=None, help="Volume percent (15..85)")
    drv_set.add_argument("--ttl", type=float, default=6.0, help="Override TTL in hours (1..72)")
    drv_set.add_argument("--config", type=Path)

    drv_clear = drv_sub.add_parser("clear", help="Clear any active drive plan override")
    drv_clear.add_argument("--config", type=Path)

    args = ap.parse_args(argv)
    cfg = load_config(args.config)
    if args.cmd == "drive":
        store = FleetDataStore(cfg)
        if args.drive_cmd == "brief":
            doc = store.build_drive_brief(shuffle=args.shuffle)
            if args.json:
                print(json.dumps(doc, ensure_ascii=False))
            else:
                print(doc["summary"])
                print("waze:", doc["waze_url"])
                print("music:", doc["music"]["station_id"], f"({doc['music']['url']})")
            return 0
        if args.drive_cmd == "set":
            payload: dict[str, Any] = {
                "ttl_hours": args.ttl,
                "place_id": args.place or None,
                "destination_name": args.name or None,
                "destination_query": args.query or None,
                "station_id": args.station or None,
                "music_mood": args.mood or None,
                "volume_pct": args.volume,
            }
            code, doc = store.action_drive_plan(payload)
            print(json.dumps(doc, ensure_ascii=False))
            return 0 if code == 200 else 1
        if args.drive_cmd == "clear":
            code, doc = store.action_drive_plan({"clear": True})
            print(json.dumps(doc, ensure_ascii=False))
            return 0 if code == 200 else 1
        return 1
    if args.cmd == "services":
        doc = FleetDataStore(cfg).build_services()
        query = args.query.casefold()
        doc["services"] = [row for row in doc["services"] if query in
                           " ".join([row["label"], row["project"], row["purpose"], row["expected"],
                                     *row["observed"]]).casefold()]
        if args.json:
            print(json.dumps(doc, ensure_ascii=False))
        else:
            if not doc["catalogAvailable"]:
                print("Job definitions unavailable", file=sys.stderr)
            for host, at in doc["observations"].items():
                print(f"{host}: observed {at or 'at unknown time'} (host local)")
            for row in doc["services"]:
                seen = ", ".join(f"{host} {obs['state']}" for host, obs in row["observed"].items())
                print(f"{row['label']}  [{row['project'] or 'unassigned'}]  "
                      f"expected: {row['expected'] or 'unspecified'}  observed: {seen or 'none'}")
                if row["purpose"]:
                    print(f"  {row['purpose']}")
        return 0 if doc["catalogAvailable"] or doc["observations"] else 1
    bind = "127.0.0.1" if cfg["bind"] == "localhost" else str(cfg["bind"])
    try:
        loopback = ipaddress.IPv4Address(bind).is_loopback
    except ValueError:
        loopback = False
    if not loopback:
        ap.error("fleet-api requires an IPv4 loopback bind behind tailscale serve")
    token = load_token(cfg)
    store = FleetDataStore(cfg)
    server = FleetHTTPServer((bind, int(cfg["port"])), store, token, cfg)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
