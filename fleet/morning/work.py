"""File warn/fail rows as one open work item per key."""
import re
from pathlib import Path


def work_path(root):
    """Cowork's inbox when that board exists, otherwise the taskhub item file."""
    root = Path(root)
    if (root / "cowork" / "board.json").is_file():
        return root / "cowork" / "inbox.json"
    if (root / "board.json").is_file():
        return root / "inbox.json"
    return root / "taskhub" / "items.json"


def file_items(findings, existing, now):
    """Return the item list. The same key updates one open row; a pass closes it."""
    by = {}
    order = []
    for item in existing or []:
        key = item.get("key")
        if not key or key in by:
            continue
        order.append(key)
        by[key] = dict(item)
    for finding in findings:
        if finding.status in ("pass", "info"):
            row = by.get(finding.key)
            if row and row.get("state") == "open":
                row["state"] = "done"
                row["status"] = finding.status
                row["updated"] = now
                row["detail"] = finding.detail
            continue
        if finding.status not in ("warn", "fail"):
            continue
        current = by.get(finding.key)
        if current:
            opened = current.get("opened", now)
            current.update(_body(finding, now))
            current["opened"] = opened
            current["state"] = "open"
            continue
        order.append(finding.key)
        by[finding.key] = _body(finding, now)
    return [by[key] for key in order]


def _body(finding, now):
    return {
        "key": finding.key,
        "work": "morning-" + _sanitize(finding.key),
        "status": finding.status,
        "state": "open",
        "title": f"{finding.name} {finding.value}".strip(),
        "detail": finding.detail,
        "scope": [f"service:{finding.area}"],
        "source": "fleet-morning",
        "opened": now,
        "updated": now,
    }


def _sanitize(text):
    cleaned = re.sub(r"[^A-Za-z0-9._+-]", "-", str(text or "")).strip(".-")
    return cleaned or "work"
