#!/usr/bin/env python3
"""Authenticated tailnet-only JSON API and OTA server for the Fleet iOS app."""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import hashlib
import hmac
import html
import json
import os
import plistlib
import re
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


class FleetDataStore:
    """Reads synced Fleet state files directly from disk without spawning subprocesses."""

    def __init__(self, cfg: dict[str, Any]) -> None:
        self.cfg = cfg
        self.home = _expand(cfg["home_dir"])
        self.mac_sync = _expand(cfg["mac_sync_dir"])
        self.evidence_dir = _expand(cfg["evidence_dir"])
        self.ota_dir = _expand(cfg["ota_dir"])
        self.audit_log = _expand(cfg["audit_log"])
        self.started_at = time.monotonic()
        self._refresh_lock = threading.Lock()
        self._refresh_running = False

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
        inbox = _read_jsonl(self.home / ".local/state/agent-mail/inbox.jsonl", limit=60)
        inbox.reverse()
        snaps = self._host_snapshots()
        unread_by_host: dict[str, int] = {}
        for hid, doc in snaps.items():
            mail_list = (doc.get("reading") or {}).get("mail") or []
            unread_by_host[hid] = sum(1 for m in mail_list if isinstance(m, dict) and not m.get("read"))

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
            "claims": claims_out,
        }

    def build_morning(self) -> dict[str, Any]:
        summary = self._morning_summary()
        return {
            "version": summary.get("version", 1),
            "generated": summary.get("generated"),
            "date": summary.get("date"),
            "ok": bool(summary.get("ok", False)),
            "counts": summary.get("counts") or {"pass": 0, "warn": 0, "fail": 0},
            "cadence": summary.get("cadence") or {},
            "agy_morning": self._agy_morning_text(),
            "rows": summary.get("rows") or [],
        }

    def build_overview(self) -> dict[str, Any]:
        morning = self._morning_summary()
        hosts_doc = self.build_hosts()
        capacity = self.build_capacity()
        backups = self.build_backups()
        sec = self.build_security()
        mail = self.build_mail()
        total_unread = sum(mail["unread_by_host"].values()) if mail["unread_by_host"] else len(mail["messages"])
        return {
            "generated_at": morning.get("generated") or int(time.time()),
            "date": morning.get("date") or _utc_iso()[:10],
            "counts": morning.get("counts") or {"pass": 0, "warn": 0, "fail": 0},
            "cadence": morning.get("cadence") or {},
            "agy_morning": self._agy_morning_text(),
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
                "rows": sec["rows"],
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
        self._audit("provider-pause", {"provider": provider, "paused": paused_val})
        return 200, {"ok": True, "provider": provider, "paused": paused_val, "list": self._paused_providers()}


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
        sys.stderr.write(f"[{_utc_iso()}] {msg}\n")

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

    def _effective_public_base(self) -> str:
        base = str(self.srv.cfg.get("public_base") or "").rstrip("/")
        if base and "example.ts.net" not in base:
            return base
        host = self.headers.get("X-Forwarded-Host") or self.headers.get("Host") or "127.0.0.1:8694"
        proto = self.headers.get("X-Forwarded-Proto") or ("https" if ".ts.net" in host else "http")
        return f"{proto}://{host}"

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

        if is_ota:
            self._serve_ota(path, tok, head_only=head_only)
            return

        self._send_json(HTTPStatus.NOT_FOUND, {"ok": False, "error": "not found"}, head_only=head_only)

    def _serve_ota(self, path: str, tok: str, head_only: bool) -> None:
        base = self._effective_public_base()
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

        length = int(self.headers.get("Content-Length", "0") or 0)
        if length < 0 or length > MAX_BODY_BYTES:
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
        else:
            code, body = HTTPStatus.NOT_FOUND, {"ok": False, "error": "unknown action"}
        self._send_json(code, body)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Fleet tailnet JSON API & OTA server")
    sub = ap.add_subparsers(dest="cmd", required=True)
    srv = sub.add_parser("serve", help="Run the HTTP server")
    srv.add_argument("--config", type=Path, default=None)
    args = ap.parse_args(argv)
    cfg = load_config(args.config)
    token = load_token(cfg)
    store = FleetDataStore(cfg)
    server = FleetHTTPServer((str(cfg["bind"]), int(cfg["port"])), store, token, cfg)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
