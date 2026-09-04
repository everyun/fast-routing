#!/usr/bin/env python3
"""Cross-check constant tables in docs/upstream-study/*.md.

The nine specs each carry Markdown tables of the form ``constant | value``
(and close variants: Const/Knob, Value/Default/release value). The
release-jar conformance section at the end of a spec is scanned the same
way, plus inline ``NAME = number`` assignments.

When the same constant appears in more than one spec, the values must
agree after light normalisation (markdown fences, numeric underscores,
Java float suffixes, ``Integer.MAX_VALUE / 100``). Conflicts are printed;
this tool does **not** edit the specs.

Finding conflicts is the job — that is a successful run (exit 0). Pass
``--strict`` to exit 1 when any conflict exists.

    python3 bench/tools/spec_lint.py
    python3 bench/tools/spec_lint.py --json
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections import defaultdict

SPEC_DIR_DEFAULT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
    "docs",
    "upstream-study",
)

NAME_HEADERS = {
    "constant",
    "const",
    "knob",
}
VALUE_HEADERS = {
    "value",
    "default",
    "release value",
    "value / default",
    "val",
}
# Prefer the jar-verified column when a conformance table has both.
VALUE_PRIORITY = (
    "release value",
    "value",
    "value / default",
    "default",
    "val",
)

IDENT_RE = re.compile(r"`([A-Za-z_][A-Za-z0-9_.]*)`")
# Specs write constants as `NAME` = 123. Bare `maxPasses = 0` in a Java listing
# is a call-site, not a constant declaration.
ASSIGN_RE = re.compile(
    r"`([A-Za-z_][A-Za-z0-9_]{2,})`\s*=\s*"
    r"`?([+-]?(?:Integer\.MAX_VALUE(?:\s*/\s*100)?|[0-9][0-9_]*(?:\.[0-9]+)?[fFdDlL]?))`?"
)
# `minEscapeLengthMm = 2.5` (name and value inside one backtick span)
INNER_ASSIGN_RE = re.compile(
    r"`([A-Za-z_][A-Za-z0-9_]{2,})\s*=\s*"
    r"([+-]?(?:Integer\.MAX_VALUE(?:\s*/\s*100)?|[0-9][0-9_]*(?:\.[0-9]+)?[fFdDlL]?))`"
)
# `minEscapeLengthMm` default **500**
DEFAULT_RE = re.compile(
    r"`([A-Za-z_][A-Za-z0-9_]{2,})`\s+default\s+\*?\*?([0-9][0-9_]*(?:\.[0-9]+)?)"
)
CONFORMANCE_HEADING_RE = re.compile(
    r"^#{2,3}\s+release-jar conformance\b",
    re.IGNORECASE,
)
MAX_INT_OVER_100 = 21474836  # Integer.MAX_VALUE / 100


def split_row(line: str) -> list[str] | None:
    s = line.strip()
    if not s.startswith("|"):
        return None
    # Trailing pipe is optional.
    body = s.strip("|")
    cells = [c.strip() for c in body.split("|")]
    return cells


def is_separator(cells: list[str]) -> bool:
    if not cells:
        return False
    return all(re.fullmatch(r":?-{3,}:?", c.replace(" ", "")) or c == "" for c in cells)


def header_key(cell: str) -> str:
    return re.sub(r"\s+", " ", cell.strip().strip("*").strip("`")).lower()


def looks_like_identifier(text: str) -> bool:
    t = text.strip().strip("`").strip("*")
    return bool(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.]*", t))


def extract_name(cell: str) -> str | None:
    """Single identifier from a name cell. Ambiguous cells are skipped."""
    ticks = IDENT_RE.findall(cell)
    if len(ticks) > 1:
        return None
    if len(ticks) == 1:
        return canonical_name(ticks[0])
    stripped = re.sub(r"[*_]", "", cell).strip()
    stripped = re.split(r"\s+(?:→|->)\s+", stripped)[0]
    stripped = re.sub(r"\s*\(.*$", "", stripped).strip()
    if looks_like_identifier(stripped):
        return canonical_name(stripped)
    return None


def canonical_name(name: str) -> str:
    name = name.strip().strip("`")
    if "." in name:
        last = name.rsplit(".", 1)[-1]
        if last.isupper() or "_" in last:
            return last
    return name


def strip_md(text: str) -> str:
    t = text.strip()
    t = t.replace("**", "").replace("__", "")
    t = t.replace("`", "")
    t = re.sub(r"</?[^>]+>", "", t)
    return t.strip()


_UNIT_RE = re.compile(
    r"^\s*([+-]?(?:Integer\.MAX_VALUE(?:\s*/\s*100)?|[0-9][0-9_]*(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?[fFdDlL]?))"
    r"(?:\s*(?:ms|s|um|µm|mm|f|d|L))?\s*$",
    re.I,
)


def parse_scalar(text: str) -> str | None:
    """Canonical numeric string, or None if this is not a scalar constant."""
    t = strip_md(text)
    t = re.sub(r"\s+", " ", t).strip()
    t = re.split(r"\s+\(", t, maxsplit=1)[0].strip()
    t = re.sub(r"\s+(internal units|board units)\s*$", "", t, flags=re.I)
    m = _UNIT_RE.match(t)
    if not m:
        compact = t.replace(" ", "")
        compact = re.sub(
            r"Integer\.MAX_VALUE\s*/\s*100", str(MAX_INT_OVER_100), compact, flags=re.I
        )
        if re.fullmatch(r"Integer\.MAX_VALUE", compact, re.I):
            return "2147483647"
        m = re.fullmatch(
            r"([+-]?)([0-9][0-9_]*(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)[fFdDlL]?", compact
        )
        if not m:
            return None
        sign, num = m.group(1), m.group(2).replace("_", "")
    else:
        token = m.group(1).replace(" ", "")
        token = re.sub(
            r"Integer\.MAX_VALUE\s*/\s*100", str(MAX_INT_OVER_100), token, flags=re.I
        )
        if re.fullmatch(r"Integer\.MAX_VALUE", token, re.I):
            return "2147483647"
        m = re.fullmatch(
            r"([+-]?)([0-9][0-9_]*(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)[fFdDlL]?", token
        )
        if not m:
            return None
        sign, num = m.group(1), m.group(2).replace("_", "")
    try:
        if "." in num or "e" in num.lower():
            val = float(sign + num)
            if val.is_integer() and abs(val) < 1e15:
                return str(int(val))
            return repr(val)
        return str(int(sign + num, 10))
    except ValueError:
        return None


def normalize_value(raw: str) -> tuple[str, str]:
    """Return (display, comparable). Comparable is a scalar string or a fallback."""
    text = strip_md(raw)
    text = re.sub(r"\s+", " ", text).strip()
    scalar = parse_scalar(raw)
    if scalar is not None:
        return text, scalar
    compact = text.replace(" ", "").replace("×", "*").replace("·", "*").lower()
    return text, compact


def parse_tables(lines: list[str], source: str) -> list[dict]:
    observations: list[dict] = []
    i = 0
    n = len(lines)
    while i < n:
        cells = split_row(lines[i])
        if cells is None:
            i += 1
            continue
        # Need a separator on the next line to be a table.
        if i + 1 >= n:
            break
        sep = split_row(lines[i + 1])
        if sep is None or not is_separator(sep):
            i += 1
            continue
        headers = [header_key(c) for c in cells]
        name_idx = next((k for k, h in enumerate(headers) if h in NAME_HEADERS), None)
        value_idx = None
        for cand in VALUE_PRIORITY:
            if cand in headers:
                value_idx = headers.index(cand)
                break
        start = i
        i += 2
        rows = []
        while i < n:
            row = split_row(lines[i])
            if row is None or is_separator(row):
                break
            rows.append((i + 1, row))  # 1-based
            i += 1
        if name_idx is None or value_idx is None:
            continue
        for line_no, row in rows:
            if name_idx >= len(row) or value_idx >= len(row):
                continue
            observations.extend(parse_inlines("|".join(row), source, line_no))
            name = extract_name(row[name_idx])
            if not name:
                continue
            raw_val = row[value_idx]
            if not strip_md(raw_val):
                continue
            display, comparable = normalize_value(raw_val)
            observations.append(
                {
                    "name": name,
                    "value_raw": raw_val,
                    "value": display,
                    "comparable": comparable,
                    "source": source,
                    "line": line_no,
                    "kind": "table",
                }
            )
        # i already at first non-row
        if i == start:
            i += 1
    return observations


def _obs(name: str, raw: str, source: str, line: int, kind: str) -> dict | None:
    name = canonical_name(name)
    if len(name) < 3 or name.lower() in {"the", "and", "for", "not", "jar"}:
        return None
    display, comparable = normalize_value(raw)
    return {
        "name": name,
        "value_raw": raw,
        "value": display,
        "comparable": comparable,
        "source": source,
        "line": line,
        "kind": kind,
    }


def parse_inlines(text: str, source: str, start_line: int) -> list[dict]:
    observations: list[dict] = []
    for match in ASSIGN_RE.finditer(text):
        item = _obs(match.group(1), match.group(2), source,
                    start_line + text[: match.start()].count("\n"), "assignment")
        if item:
            observations.append(item)
    for match in INNER_ASSIGN_RE.finditer(text):
        item = _obs(match.group(1), match.group(2), source,
                    start_line + text[: match.start()].count("\n"), "assignment")
        if item:
            observations.append(item)
    for match in DEFAULT_RE.finditer(text):
        item = _obs(match.group(1), match.group(2), source,
                    start_line + text[: match.start()].count("\n"), "default")
        if item:
            observations.append(item)
    return observations


def split_conformance(text: str) -> tuple[str, str, int]:
    """Return (body, conformance, conformance_start_line)."""
    lines = text.splitlines()
    for idx, line in enumerate(lines):
        if CONFORMANCE_HEADING_RE.match(line.strip()):
            start = idx
            return "\n".join(lines[:start]), "\n".join(lines[start:]), start + 1
    return text, "", 0


def collect_from_markdown(text: str, source: str) -> list[dict]:
    body, conf, conf_start = split_conformance(text)
    observations: list[dict] = []
    observations.extend(parse_tables(body.splitlines(), source))
    if conf:
        observations.extend(parse_tables(conf.splitlines(), source))
        observations.extend(parse_inlines(conf, source, conf_start))
    return observations


def group_observations(obs: list[dict]) -> dict[str, list[dict]]:
    grouped: dict[str, list[dict]] = defaultdict(list)
    for item in obs:
        grouped[item["name"]].append(item)
    return grouped


def _is_scalar(item: dict) -> bool:
    return parse_scalar(item["value_raw"]) is not None


def find_conflicts(grouped: dict[str, list[dict]]) -> list[dict]:
    conflicts = []
    for name, items in sorted(grouped.items()):
        # Only scalar observations participate: prose ("see maze spec",
        # "20 / 5 / (ctrl)") is not a value we can usefully compare.
        scalar_items = [it for it in items if _is_scalar(it)]
        files = {it["source"] for it in scalar_items}
        if len(files) < 2:
            continue
        by_value: dict[str, list[dict]] = defaultdict(list)
        for it in scalar_items:
            by_value[it["comparable"]].append(it)
        if len(by_value) < 2:
            continue
        conflicts.append(
            {
                "name": name,
                "values": [
                    {
                        "comparable": val,
                        "examples": [
                            {
                                "source": it["source"],
                                "line": it["line"],
                                "value": it["value"],
                                "kind": it["kind"],
                            }
                            for it in group
                        ],
                    }
                    for val, group in sorted(by_value.items(), key=lambda kv: kv[0])
                ],
            }
        )
    return conflicts


def lint_dir(spec_dir: str) -> dict:
    observations: list[dict] = []
    files = []
    if not os.path.isdir(spec_dir):
        return {
            "ok": False,
            "error": f"spec dir not found: {spec_dir}",
            "files": [],
            "observations": [],
            "conflicts": [],
        }
    for name in sorted(os.listdir(spec_dir)):
        if not name.endswith(".md"):
            continue
        path = os.path.join(spec_dir, name)
        files.append(name)
        with open(path, encoding="utf-8") as f:
            text = f.read()
        observations.extend(collect_from_markdown(text, name))
    grouped = group_observations(observations)
    conflicts = find_conflicts(grouped)
    return {
        "ok": True,
        "files": files,
        "observation_count": len(observations),
        "names": len(grouped),
        "conflicts": conflicts,
        "observations": observations,
    }


def format_report(result: dict) -> str:
    lines = []
    if not result.get("ok"):
        lines.append(f"error: {result.get('error')}")
        return "\n".join(lines)
    lines.append(
        f"specs: {len(result['files'])}  observations: {result['observation_count']}  "
        f"names: {result['names']}  conflicts: {len(result['conflicts'])}"
    )
    if not result["conflicts"]:
        lines.append("no cross-spec constant conflicts")
        return "\n".join(lines)
    lines.append("")
    lines.append("CONFLICTS")
    for c in result["conflicts"]:
        lines.append(f"- {c['name']}")
        for val in c["values"]:
            shown = ", ".join(
                f"{ex['source']}:{ex['line']} = {ex['value']!r}"
                for ex in val["examples"][:4]
            )
            extra = len(val["examples"]) - 4
            if extra > 0:
                shown += f" (+{extra} more)"
            lines.append(f"    {val['comparable']!r}: {shown}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dir", default=SPEC_DIR_DEFAULT)
    ap.add_argument("--json", action="store_true")
    ap.add_argument(
        "--strict",
        action="store_true",
        help="exit 1 when any cross-spec conflict is found",
    )
    args = ap.parse_args(argv)
    result = lint_dir(args.dir)
    public = dict(result)
    # Keep JSON smaller unless the caller asks for raw observations.
    public.pop("observations", None)
    if args.json:
        print(json.dumps(public, indent=2, ensure_ascii=False))
    else:
        print(format_report(result))
    if not result.get("ok"):
        return 2
    if args.strict and result["conflicts"]:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
