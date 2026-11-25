"""Tripwire's report contract and serialized, atomic publication."""
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import tempfile
import stat
import uuid


BENIGN = "VERDICT: BENIGN"
NEEDS_REVIEW = "VERDICT: NEEDS REVIEW"
PROVIDER_VERDICTS = {BENIGN, "VERDICT: SUSPICIOUS", "VERDICT: HOSTILE"}


def verdict(text):
    lines = text.splitlines()
    if (lines and lines[0] in PROVIDER_VERDICTS | {NEEDS_REVIEW}
            and any(line.strip() for line in lines[1:])):
        return lines[0]
    return None


def saved_verdict(incident):
    try:
        fd = os.open(incident / "report.md", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError:
        return None
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        return None
    with open(fd, encoding="utf-8", errors="replace") as stream:
        first = stream.readline(80).rstrip("\n")
        if first not in PROVIDER_VERDICTS | {NEEDS_REVIEW}:
            return None
        while True:
            data = stream.read(8192)
            if not data:
                return None
            if data.strip():
                return first


@contextmanager
def locked(directory, name):
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / name).open("a") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        yield


def publish(incident, text):
    with locked(incident, "report.lock"):
        return publish_locked(incident, text)


def publish_locked(incident, text):
    """Keep the first complete report. Retain partial replies intact for diagnosis."""
    new = verdict(text)
    if new is None:
        raise ValueError("incomplete Tripwire report")
    path = incident / "report.md"
    existing = saved_verdict(incident)
    # A late adverse finding from an investigator still matters after local triage.
    if existing and not (existing == NEEDS_REVIEW and new in {"VERDICT: SUSPICIOUS", "VERDICT: HOSTILE"}):
        return existing
    with tempfile.TemporaryDirectory(dir=incident) as tmp:
        stage = Path(tmp) / "report.md"
        with open(os.open(stage, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "w", encoding="utf-8") as stream:
            stream.write(text.rstrip() + "\n")
        if existing:
            os.link(path, incident / f"report.local-{uuid.uuid4().hex}.md")
            os.replace(stage, path)
            return new
        if os.path.lexists(path):
            os.rename(path, incident / f"report.incomplete-{uuid.uuid4().hex}.md")
        try:
            os.link(stage, path)
        except FileExistsError:  # An independent human report won the race.
            return saved_verdict(incident)
    return new


def local_report(incident, record, reason, current):
    """Report recorded facts and observations. Neither establishes authorization."""
    lines = [NEEDS_REVIEW, "", "Tripwire could not verify who authorized the changes.",
             f"Investigation outcome: {reason}.", "", "Recorded observations:"]
    for change in record.get("changes", []):
        wire, key = change["wire"], change["what"]
        entry = {"wire": wire, "name": key, "before": change.get("before"), "after": change.get("after")}
        sampled = current.get(wire)
        if isinstance(sampled, dict):
            entry["observed_at_review"] = sampled.get(key)
        lines.append("    " + json.dumps(entry, ensure_ascii=True))
    for attempt in ("primary", "fallback"):
        try:
            result = json.loads((incident / f"{attempt}-result.json").read_text())
        except (OSError, ValueError):
            continue
        lines.extend(["", attempt.capitalize() + " attempt: " + json.dumps(result, ensure_ascii=True)])
    files = [h for h in record.get("holds", []) if h.get("kind") == "file"]
    if files and not record.get("released"):
        lines.append(f"\n{len(files)} file(s) stay put back pending owner review.")
    lines.extend(["", "Compare the changes with the originating job, approved work or signed installation record.",
                  "incident.json contains the recorded changes and hold disposition."])
    return "\n".join(lines)
