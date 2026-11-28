"""Tripwire's report contract and serialized, atomic publication."""
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import tempfile
import stat
import uuid

import tripwire_seal as seal


BENIGN = "VERDICT: BENIGN"
NEEDS_REVIEW = "VERDICT: NEEDS REVIEW"
PROVIDER_VERDICTS = {BENIGN, "VERDICT: SUSPICIOUS", "VERDICT: HOSTILE"}


def verdict(text):
    lines = text.splitlines()
    if (lines and lines[0] in PROVIDER_VERDICTS | {NEEDS_REVIEW}
            and any(line.strip() for line in lines[1:])):
        return lines[0]
    return None


def read_report(incident, key=None):
    """(verdict, sealed) from ONE read of report.md, opened without following a link. The
    verdict and the seal are judged over the same bytes, so a file swapped between two looks
    cannot pass with one report's seal and another's verdict. Anything can write report.md;
    only a holder of the key can make the seal, which covers the incident's name and the bytes."""
    try:
        fd = os.open(incident / "report.md", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError:
        return None, False
    with open(fd, "rb") as stream:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            return None, False
        data = stream.read()
    first, _, rest = data.decode("utf-8", "replace").partition("\n")
    if first not in PROVIDER_VERDICTS | {NEEDS_REVIEW} or not rest.strip():
        return None, False
    try:
        mark = (incident / "report.seal").read_text(errors="replace").strip()
    except OSError:
        mark = ""
    return first, bool(key) and seal.same(mark, seal.mac(key, "report", incident.name, seal.sha(data)))


def saved_verdict(incident):
    """The verdict report.md states, whoever wrote it: enough to tell the owner, never to act on."""
    return read_report(incident)[0]


def sealed_verdict(incident, key):
    """The verdict, only from a sealed report: the one a hold may be released on."""
    first, signed = read_report(incident, key)
    return first if signed else None


def saved_form(text):
    return (text.rstrip() + "\n").encode("utf-8")


def write_seal(incident, key, text):
    """Seal the bytes this process published, not whatever the file holds a moment later."""
    with tempfile.TemporaryDirectory(dir=incident) as tmp:
        stage = Path(tmp) / "report.seal"
        stage.write_text(seal.mac(key, "report", incident.name, seal.sha(saved_form(text))) + "\n")
        os.replace(stage, incident / "report.seal")


@contextmanager
def locked(directory, name):
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / name).open("a") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        yield


def publish(incident, text, key=None):
    with locked(incident, "report.lock"):
        return publish_locked(incident, text, key)


def publish_locked(incident, text, key=None):
    """Keep the first complete report. Retain partial replies intact for diagnosis.
    With a key the report is sealed, and a report already there without a seal is set aside:
    nobody tripwire started wrote it, so it must not stand in the way of the one that did."""
    new = verdict(text)
    if new is None:
        raise ValueError("incomplete Tripwire report")
    path = incident / "report.md"
    existing, signed = read_report(incident, key)
    if existing and key and not signed:
        os.rename(path, incident / f"report.unsigned-{uuid.uuid4().hex}.md")
        existing = None
    new, wrote = _publish(incident, path, text, new, existing)
    if key and wrote:
        write_seal(incident, key, text)
    return new


def _publish(incident, path, text, new, existing):
    """(the verdict that stands, whether this call wrote the report that carries it)."""
    # A late adverse finding from an investigator still matters after local triage.
    if existing and not (existing == NEEDS_REVIEW and new in {"VERDICT: SUSPICIOUS", "VERDICT: HOSTILE"}):
        return existing, False
    with tempfile.TemporaryDirectory(dir=incident) as tmp:
        stage = Path(tmp) / "report.md"
        with open(os.open(stage, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "wb") as stream:
            stream.write(saved_form(text))
        if existing:
            os.link(path, incident / f"report.local-{uuid.uuid4().hex}.md")
            os.replace(stage, path)
            return new, True
        if os.path.lexists(path):
            os.rename(path, incident / f"report.incomplete-{uuid.uuid4().hex}.md")
        try:
            os.link(stage, path)
        except FileExistsError:  # An independent human report won the race.
            return saved_verdict(incident), False
    return new, True


def local_report(incident, record, reason, current, receipts=None):
    """Report recorded facts and observations. Neither establishes authorization.
    `receipts` are the attempts' outcomes as tripwire sealed them, by attempt name."""
    lines = [NEEDS_REVIEW, "", "Tripwire could not verify who authorized the changes.",
             f"Investigation outcome: {reason}.", "", "Recorded observations:"]
    for change in record.get("changes", []):
        wire, key = change["wire"], change["what"]
        entry = {"wire": wire, "name": key, "before": change.get("before"), "after": change.get("after")}
        sampled = current.get(wire)
        if isinstance(sampled, dict):
            entry["observed_at_review"] = sampled.get(key)
        lines.append("    " + json.dumps(entry, ensure_ascii=True))
    for attempt, result in (receipts or {}).items():
        if result is not None:
            lines.extend(["", attempt.capitalize() + " attempt: " + json.dumps(result, ensure_ascii=True)])
    files = [h for h in record.get("holds", []) if h.get("kind") == "file"]
    if files and not record.get("released"):
        lines.append(f"\n{len(files)} file(s) stay put back pending owner review.")
    lines.extend(["", "Compare the changes with the originating job, approved work or signed installation record.",
                  "incident.json contains the recorded changes and hold disposition."])
    return "\n".join(lines)
