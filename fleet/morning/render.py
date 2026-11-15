"""One instrument row per finding that is not a pass. Warn and fail details follow."""

from fleet.morning.evaluate import ORDER

_RANK = {"fail": 0, "warn": 1, "info": 2, "pass": 3}


def render(findings, generated, date):
    ranked = sorted(findings, key=lambda item: (
        _RANK.get(item.status, 9),
        ORDER.index(item.area) if item.area in ORDER else 99,
        item.name,
        item.key,
    ))
    shown = [item for item in ranked if item.status != "pass"]
    passed = sum(item.status == "pass" for item in ranked)
    instruments = _block(shown)
    if passed:
        instruments.append(f"* {passed} checks passed")
    details = []
    for item in ranked:
        if item.status not in ("warn", "fail"):
            continue
        detail = " ".join(str(item.detail or "").split())
        details.append(f"{item.symbol} {item.key}  {detail}".rstrip())
    lines = list(instruments)
    if details:
        lines.append("")
        lines.extend(details)
    text = "\n".join(lines) + ("\n" if lines else "")
    counts = {"pass": 0, "warn": 0, "fail": 0}
    rows = []
    for item in ranked:
        if item.status in counts:
            counts[item.status] += 1
        rows.append({
            "key": item.key,
            "status": item.status,
            "symbol": item.symbol,
            "area": item.area,
            "name": item.name,
            "value": item.value,
            "datum": item.datum,
            "detail": item.detail,
        })
    document = {
        "version": 1,
        "generated": int(generated),
        "date": date,
        "ok": counts["warn"] == 0 and counts["fail"] == 0,
        "counts": {"pass": counts["pass"], "warn": counts["warn"], "fail": counts["fail"]},
        "rows": rows,
    }
    return text, document


def instrument_line(item):
    """One full row. Names are not clipped; the block pads columns to the row."""
    return _block([item])[0]


def _block(items):
    if not items:
        return []
    areas = [" ".join(str(item.area or "").split()) for item in items]
    names = [" ".join(str(item.name or "").split()) for item in items]
    values = [" ".join(str(item.value or "").split()) for item in items]
    area_w = max(len(text) for text in areas)
    name_w = max(len(text) for text in names)
    value_w = max(len(text) for text in values)
    lines = []
    for item, area, name, value in zip(items, areas, names, values):
        datum = " ".join(str(item.datum or "").split())
        lines.append(f"{item.symbol} {area:<{area_w}} {name:<{name_w}} {value:>{value_w}}  {datum}".rstrip())
    return lines


def notification_text(findings):
    bad = [item for item in findings if item.status in ("warn", "fail")]
    if not bad:
        return None
    fails = sum(item.status == "fail" for item in bad)
    warns = len(bad) - fails
    worst = sorted(bad, key=lambda item: item.status != "fail")[0]
    text = f"{worst.symbol} {fails} fail {warns} warn · {worst.name} {worst.value}"
    text = " ".join(text.split())
    return text[:80]
