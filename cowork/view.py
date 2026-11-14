"""One instrument row per claim or unpaired session.

Columns are as wide as their contents. A scope is never cut down to an ellipsis.
"""
from cowork.model import age, display_owner, scope_label, visible_state

COLUMNS = (
    ("work", "left"),
    ("owner", "left"),
    ("host", "left"),
    ("scope", "left"),
    ("state", "left"),
    ("heartbeat", "right"),
    ("conflict", "left"),
)

SYMBOL = {
    "active": "*",
    "stale": "~",
    "released": "·",
    "done": "✓",
    "handed-off": "→",
}

# Colour is paired with the symbol. Bright enough for a light or a dark terminal.
PAINT = {
    "*": "\033[1m",
    "~": "\033[1;33m",
    "·": "\033[2m",
    "✓": "\033[1;32m",
    "→": "\033[1;36m",
    "!": "\033[1;31m",
    "○": "\033[1;33m",
}
RESET = "\033[0m"


_CONTROL = {c: " " for c in list(range(0x00, 0x20)) + [0x7f] + list(range(0x80, 0xa0))}


def clean(text):
    """Board text comes from other agents: drop C0/C1 controls (ESC, CSI) so a value cannot drive the terminal."""
    return str(text).translate(_CONTROL)


def _fit(text, width, align):
    text = clean(text)
    if align == "right":
        return text.rjust(width)
    return text.ljust(width)


def paint(text, plain):
    if plain or not text:
        return text
    mark = text[0]
    colour = PAINT.get(mark)
    if not colour:
        return text
    return colour + text + RESET


def _widths(rows):
    widths = [len(name) for name, _align in COLUMNS]
    for row in rows:
        for index, value in enumerate(row):
            widths[index] = max(widths[index], len(str(value).replace("\n", " ")))
    return widths


def format_table(rows, plain=True):
    widths = _widths(rows)
    lines = [" ".join(_fit(name, width, "left") for (name, _align), width in zip(COLUMNS, widths)).rstrip()]
    for row in rows:
        lines.append(_join(row, plain, widths))
    return "\n".join(lines) + "\n"


def _join(row, plain, widths):
    parts = []
    for (name, align), value, width in zip(COLUMNS, row, widths):
        fitted = _fit(value, width, align)
        if not plain and name == "state" and fitted.strip():
            fitted = paint(fitted.lstrip(), plain=False)
            fitted = fitted + ""  # symbol keeps its own reset
            # Repad using the visible width (codes are zero-width to the reader,
            # but a following column is aligned on the unpainted row). The screen
            # shot path paints after alignment; tests use plain.
            fitted = _fit(value, width, align)
            fitted = _paint_symbol(fitted)
        elif not plain and name == "conflict" and fitted.strip().startswith("!"):
            fitted = _paint_symbol(fitted)
        parts.append(fitted)
    return " ".join(parts).rstrip()


def _paint_symbol(fitted):
    text = fitted.lstrip()
    if not text:
        return fitted
    mark = text[0]
    colour = PAINT.get(mark)
    if not colour:
        return fitted
    prefix = fitted[: len(fitted) - len(text)]
    return prefix + colour + mark + RESET + text[1:]


def claim_cells(claim, now):
    state = claim.get("state") or visible_state(claim, now)
    owner = claim.get("owner") or {}
    provider = owner.get("provider") or claim.get("provider") or ""
    conflicts = claim.get("conflicts") or []
    if conflicts:
        mail = conflicts[0].get("mail") or conflicts[0].get("work") or ""
        conflict = f"! {mail}" if mail else "!"
    else:
        conflict = "·"
    return [
        claim.get("work") or "·",
        display_owner(provider, (owner.get("label") or claim.get("label") or ""), owner.get("mail") or claim.get("mail") or ""),
        owner.get("host") or claim.get("host") or "·",
        scope_label(claim.get("scope") or []),
        SYMBOL.get(state, "·"),
        age(now, claim.get("heartbeat") or 0),
        conflict,
    ]


def presence_cells(row, now):
    state = row.get("state") or "active"
    return [
        row.get("project") or "·",
        display_owner(row.get("provider") or "", row.get("label") or row.get("project") or "", ""),
        row.get("host") or "·",
        f"repo {row.get('project') or '·'}",
        SYMBOL.get(state, "·"),
        age(now, row.get("at") or 0),
        "·",
    ]


def _open_items(snapshot):
    items = [item for item in snapshot.get("items") or [] if item.get("state") == "open"]
    items.sort(key=lambda item: (int(item.get("opened") or 0), item.get("key") or ""))
    return items


def format_open(items, now, plain=True):
    """Unowned work, oldest first. One row: symbol, key, scope, age."""
    if not items:
        return ""
    body = []
    for item in items:
        body.append((
            "○",
            clean(item.get("key") or "·"),
            clean(scope_label(item.get("scope") or [])),
            "age " + age(now, item.get("opened") or 0),
        ))
    widths = [max(len(part) for part in column) for column in zip(*body)]
    lines = ["OPEN"]
    for row in body:
        cells = []
        for index, (part, width) in enumerate(zip(row, widths)):
            shown = part.rjust(width) if index == len(row) - 1 else part.ljust(width)
            if index == 0 and not plain:
                shown = _paint_symbol(shown)
            cells.append(shown)
        lines.append("  ".join(cells).rstrip())
    return "\n".join(lines)


def table_from_snapshot(snapshot, now, plain=True, only=None):
    rows = []
    claims = snapshot.get("claims") or []
    if only is not None:
        claims = [c for c in claims if c.get("work") in only or c in only]
    sessions = {((c.get("owner") or {}).get("session")) for c in claims}
    for claim in claims:
        rows.append(claim_cells(claim, now))
    if only is None:
        for row in snapshot.get("presence") or []:
            if row.get("id") in sessions:
                continue
            rows.append(presence_cells(row, now))
    text = format_table(rows, plain=plain)
    if only is not None:
        return text
    extra = format_open(_open_items(snapshot), now, plain=plain)
    if not extra:
        return text
    return text.rstrip("\n") + "\n\n" + extra + "\n"
