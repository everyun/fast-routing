#!/usr/bin/env python3
"""Generate `docs/CLI.md`: Freerouting 2.3.0 CLI vs `fr-cli`.

Status values: 兼容 / 部分 / 不支持 / 我方独有.

    python3 bench/tools/cli_compat.py --write
    python3 -m unittest discover -s bench/tools/tests -p 'test_cli_compat.py'
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DOC_PATH = REPO / "docs" / "CLI.md"
RUST_CLI = REPO / "crates" / "fr-cli" / "src" / "main.rs"

JAVA_ROOTS = [
    Path("/tmp/fr-java-release/src"),
    Path(os.path.expanduser("~/fr-ref/fr-java-release/src")),
]

# Handwritten notes only.
NOTES: dict[str, str] = {
    "-de": "Java accepts several files and dispatches by extension (dsn/json/ses/rules) and `a.dsn+b.rules`. We support multi-file and plus-separated dispatch.",
    "-di": "GUI input directory. We resolve relative design paths against `-di`.",
    "-do": "Output SES path. Default `<input>.ses` on both sides for a routing run.",
    "-dr": "Rules sidecar path. Accepted with note; design rules in the .dsn take precedence.",
    "-drc": "DRC-only mode (disables the router, writes JSON). Our `-drc` / `--drc-report` emits the same-shaped JSON.",
    "-mp": "Pass cap. Java clamps to `[0,9999]` (0 = unlimited) and a *negative* value is a latent uncapped run. We reject negatives (PARITY-DIVERGENCES).",
    "-mt": "Optimizer worker thread count, clamped `[0,1024]`. Maps to our worker thread budget (optimizer candidate evaluation + referee scan).",
    "-oit": "Optimizer improvement threshold in **percent**. Scaled to fraction and applied to optimizer.",
    "-us": "Java: optimizer board-update strategy (`greedy|global|hybrid`). We accept strategy names or numeric thread count alias for backward compatibility.",
    "-is": "Optimizer item-selection strategy (`seq|rand|prio`). Accepted and parsed.",
    "-hr": "Optimizer hybrid ratio. Accepted and parsed.",
    "-inc": "Ignore net classes. GUI-only effect upstream; headless no-op on both sides (PARITY-DIVERGENCES).",
    "-l": "Locale. Accepted; headless CLI messages are in English.",
    "-dl": "Disable file log. Accepted (file logging is off by default in headless mode).",
    "-da": "Disable analytics. Accepted (telemetry-free).",
    "-ll": "Console log level. Accepted and parsed.",
    "-host": "Host tag for analytics. Accepted (telemetry-free).",
    "-dct": "GUI dialog confirmation timeout. Accepted (no-op in headless mode).",
    "-help": "Help. We also take `-h` / `--help`.",
    "-h": "Help. Java accepts it as an alias of `-help`.",
    "--help": "Help.",
    "--compare-boards": "Java board-compare utility. Unsupported (fast-routing is a headless router engine, not a board comparison GUI tool; use `fr-check` or `kicad-cli`).",
    "--user_data_path": "Java user-data directory. Accepted for drop-in compatibility (fast-routing is stateless with no persistent JSON settings file).",
    "--debug.*": "Java debug switches. Accepted and parsed.",
    "--router.*": "Generic router setting setter (`--router.max_passes=`, `--router.optimizer.enabled=`, `--router.fanout.enabled=`, etc.). Mapped to routing configuration.",
    "--<path>=<value>": "Generic setter for any `GlobalSettings` path (`--logging.*`, `--gui.*`, etc.). Non-router subsystems are headless/no-op.",
    "--threads": "Workers for optimizer candidate evaluation and the referee scan. Not the route pass. Default 1. Unique.",
    "--portfolio": "Race N route passes on board clones; keep the best referee score. Unique.",
    "--no-optimizer": "Skip the post-route optimizer (on by default, matching DefaultSettings). Unique spelling; Java uses `--router.optimizer.enabled=false`.",
    "--fanout": "Opt-in SMD escape pre-pass. Java default is on via settings; we default on and take `--no-fanout` to disable. Unique spelling; Java uses `--router.fanout.enabled=`.",
    "--no-fanout": "Skip the SMD pin-escape pre-pass. Unique.",
    "--deterministic": "Effort-based virtual time instead of wall-clock budgets. Unique.",
    "--job-timeout": "Whole-job wall budget; on expiry write the SES and exit 4. Java has `router.job_timeout` as a settings string, not this flag.",
    "--import-only": "Import and judge; do not route. Unique.",
    "--golden-dump": "G1.1 model dump. Unique.",
    "--drc-report": "G1.2a `java -drc`-shaped JSON (long form of `-drc`).",
    "--unit": "DRC report unit. Unique.",
    "--router-overrides": "Import with routing-path post-load overrides. Unique.",
    "--emit-boxes": "Debug BOX lines on stderr. Unique.",
    "--pin-frame-fix": "Deprecated no-op. Unique leftover for `drc_compare.py`.",
    "--settle-trace": "FRMETRIC settle stream (lab-jar shape). Unique.",
    "-v": "Print version. Unique (Java has no `-v`).",
    "--version": "Print version. Unique.",
    "--max-passes": "Long form of `-mp`. Unique spelling; Java only has `-mp`.",
    "--design": "Long form of `-de`. Unique spelling.",
    "--output": "Long form of `-do`. Unique spelling.",
}


@dataclass
class Flag:
    name: str
    side: str  # java / ours / both
    java_meaning: str = ""
    ours_meaning: str = ""
    status: str = ""
    note: str = ""


_JAVA_STARTS = re.compile(
    r'p_args\[i\]\.startsWith\("(?P<s>[^"]+)"\)'
)
_JAVA_EQUALS = re.compile(
    r'p_args\[i\]\.equals(?:IgnoreCase)?\("(?P<s>[^"]+)"\)'
)


def find_global_settings() -> Path | None:
    for root in JAVA_ROOTS:
        direct = root / "main/java/app/freerouting/settings/GlobalSettings.java"
        if direct.is_file():
            return direct
        for path in root.rglob("GlobalSettings.java"):
            if "settings" in path.as_posix():
                return path
    return None


def parse_java_flags(text: str) -> list[str]:
    """Flags `applyCommandLineArguments` actually inspects, plus `--user_data_path`."""
    names: list[str] = []
    seen: set[str] = set()

    def add(raw: str) -> None:
        name = raw
        if name.startswith("--") and "=" in name:
            name = name.split("=", 1)[0]
        if name in {"-", "--", "---", "--<path>"} or name.endswith("."):
            return
        if name not in seen:
            seen.add(name)
            names.append(name)

    for match in _JAVA_EQUALS.finditer(text):
        add(match.group("s"))
    for match in _JAVA_STARTS.finditer(text):
        add(match.group("s"))
    # Special-cased before this method.
    add("--user_data_path")
    add("--debug.*")
    add("--router.*")
    add("--<path>=<value>")
    return names


def parse_ours_flags(text: str) -> list[str]:
    """Match-arm flag literals in `fr-cli`."""
    names: list[str] = []
    seen: set[str] = set()
    for match in re.finditer(r'"(?P<s>--?[A-Za-z][A-Za-z0-9_-]*)"', text):
        s = match.group("s")
        if s not in seen:
            seen.add(s)
            names.append(s)
    # Generic parameter prefixes
    names.extend(["--debug.*", "--router.*", "--<path>=<value>"])
    return names


# Canonical Java short/long flags we always want in the table even if the
# parser's startsWith grouping collapses some of them.
JAVA_CANONICAL = [
    "-de",
    "-di",
    "-do",
    "-dr",
    "-drc",
    "-mp",
    "-mt",
    "-oit",
    "-us",
    "-is",
    "-hr",
    "-inc",
    "-l",
    "-dl",
    "-da",
    "-ll",
    "-host",
    "-dct",
    "-help",
    "-h",
    "--help",
    "--compare-boards",
    "--user_data_path",
    "--debug.*",
    "--router.*",
    "--<path>=<value>",
]

OURS_CANONICAL = [
    "-de",
    "--design",
    "-di",
    "-do",
    "--output",
    "-dr",
    "-drc",
    "-mp",
    "--max-passes",
    "-mt",
    "-oit",
    "-us",
    "-is",
    "-hr",
    "-inc",
    "-l",
    "-dl",
    "-da",
    "-ll",
    "-host",
    "-dct",
    "-help",
    "-h",
    "--help",
    "--compare-boards",
    "--user_data_path",
    "--debug.*",
    "--router.*",
    "--<path>=<value>",
    "--threads",
    "--portfolio",
    "--fanout",
    "--no-fanout",
    "--no-optimizer",
    "--deterministic",
    "--job-timeout",
    "--import-only",
    "--golden-dump",
    "--drc-report",
    "--unit",
    "--router-overrides",
    "--emit-boxes",
    "--pin-frame-fix",
    "--settle-trace",
    "-v",
    "--version",
]


def classify_pair(name: str, java: set[str], ours: set[str]) -> Flag:
    flag = Flag(name=name, side="both" if name in java and name in ours else ("java" if name in java else "ours"))
    flag.note = NOTES.get(name, "")
    in_j, in_o = name in java, name in ours

    if name == "-us":
        flag.java_meaning = "optimizer board-update strategy (`greedy|global|hybrid`)"
        flag.ours_meaning = "strategy token or numeric `--threads` alias"
        flag.status = "部分"
        return flag
    if name == "-di":
        flag.java_meaning = "GUI input directory"
        flag.ours_meaning = "GUI input directory; resolves relative design paths"
        flag.status = "部分"
        return flag
    if name == "-de":
        flag.java_meaning = "one or more design files, dispatched by extension"
        flag.ours_meaning = "one or more design files (.dsn, .ses, .rules, '+')"
        flag.status = "兼容"
        return flag
    if name == "-dr":
        flag.java_meaning = "rules sidecar path"
        flag.ours_meaning = "accepted with note; rules in .dsn take precedence"
        flag.status = "部分"
        return flag
    if name == "-drc":
        flag.java_meaning = "DRC-only mode + JSON path"
        flag.ours_meaning = "DRC-only mode emitting DRC report JSON"
        flag.status = "兼容"
        return flag
    if name == "-mp":
        flag.java_meaning = "max passes, clamp [0,9999]; negative becomes uncapped"
        flag.ours_meaning = "max passes; negative rejected"
        flag.status = "部分"
        return flag
    if name == "-mt":
        flag.java_meaning = "Java optimizer thread count, clamped `[0,1024]`"
        flag.ours_meaning = "worker thread count for optimizer and referee (`[0,1024]`)"
        flag.status = "兼容"
        return flag
    if name == "-oit":
        flag.java_meaning = "optimizer improvement threshold in percent"
        flag.ours_meaning = "optimizer improvement threshold in percent (divided by 100)"
        flag.status = "兼容"
        return flag
    if name == "-is":
        flag.java_meaning = "optimizer item-selection strategy (`seq|rand|prio`)"
        flag.ours_meaning = "optimizer item-selection strategy parsed"
        flag.status = "部分"
        return flag
    if name == "-hr":
        flag.java_meaning = "optimizer hybrid ratio"
        flag.ours_meaning = "optimizer hybrid ratio parsed"
        flag.status = "部分"
        return flag
    if name == "-inc":
        flag.java_meaning = "ignore net classes (headless no-op)"
        flag.ours_meaning = "ignore net classes (headless no-op)"
        flag.status = "兼容"
        return flag
    if name == "-l":
        flag.java_meaning = "locale"
        flag.ours_meaning = "locale parsed; headless CLI output is English"
        flag.status = "部分"
        return flag
    if name == "-dl":
        flag.java_meaning = "disable file log"
        flag.ours_meaning = "disable file log (off by default in headless mode)"
        flag.status = "兼容"
        return flag
    if name == "-da":
        flag.java_meaning = "disable analytics"
        flag.ours_meaning = "disable analytics (telemetry-free)"
        flag.status = "兼容"
        return flag
    if name == "-ll":
        flag.java_meaning = "console log level"
        flag.ours_meaning = "console log level parsed"
        flag.status = "部分"
        return flag
    if name == "-host":
        flag.java_meaning = "host tag for analytics"
        flag.ours_meaning = "host tag parsed (telemetry-free)"
        flag.status = "部分"
        return flag
    if name == "-dct":
        flag.java_meaning = "GUI dialog confirmation timeout"
        flag.ours_meaning = "GUI dialog timeout parsed (no-op in headless mode)"
        flag.status = "兼容"
        return flag
    if name in {"-help", "-h", "--help"}:
        flag.java_meaning = "help"
        flag.ours_meaning = "help"
        flag.status = "兼容"
        return flag
    if name == "-do":
        flag.java_meaning = "output SES/JSON"
        flag.ours_meaning = "output SES"
        flag.status = "兼容"
        return flag
    if name == "--user_data_path":
        flag.java_meaning = "Java user-data directory"
        flag.ours_meaning = "user-data directory accepted (stateless)"
        flag.status = "兼容"
        return flag
    if name == "--compare-boards":
        flag.java_meaning = "Java board-compare utility"
        flag.ours_meaning = "unsupported with diagnostic (use `fr-check` or `kicad-cli`)"
        flag.status = "不支持"
        return flag
    if name == "--debug.*":
        flag.java_meaning = "Java debug switches"
        flag.ours_meaning = "debug properties parsed"
        flag.status = "部分"
        return flag
    if name == "--router.*":
        flag.java_meaning = "generic setter into `GlobalSettings.router`"
        flag.ours_meaning = "generic setter into router settings (`max_passes`, `optimizer.enabled`, `fanout.enabled`, etc.)"
        flag.status = "兼容"
        return flag
    if name == "--<path>=<value>":
        flag.java_meaning = "generic setter for any `GlobalSettings` path"
        flag.ours_meaning = "generic settings parsed; non-router subsystems are headless/no-op"
        flag.status = "部分"
        return flag
    if name == "--drc-report":
        flag.java_meaning = "DRC JSON report path"
        flag.ours_meaning = "G1.2a `java -drc`-shaped JSON (long form of `-drc`)"
        flag.status = "兼容"
        return flag

    if in_j and not in_o:
        flag.java_meaning = NOTES.get(name, "")
        flag.status = "不支持"
        return flag
    if in_o and not in_j:
        flag.ours_meaning = NOTES.get(name, "")
        flag.status = "我方独有"
        return flag
    flag.status = "兼容"
    return flag


def collect() -> list[Flag]:
    gs = find_global_settings()
    if gs is None:
        raise SystemExit("GlobalSettings.java not found (infra Java source missing)")
    java_text = gs.read_text(encoding="utf-8", errors="replace")
    ours_text = RUST_CLI.read_text(encoding="utf-8", errors="replace")
    parsed_java = set(parse_java_flags(java_text)) | set(JAVA_CANONICAL)
    parsed_ours = set(parse_ours_flags(ours_text)) | set(OURS_CANONICAL)
    ordered: list[str] = []
    seen: set[str] = set()
    for name in JAVA_CANONICAL + OURS_CANONICAL:
        if name not in seen:
            seen.add(name)
            ordered.append(name)
    for name in sorted(parsed_java | parsed_ours):
        if name not in seen and name.startswith("-"):
            seen.add(name)
            ordered.append(name)
    return [classify_pair(name, parsed_java, parsed_ours) for name in ordered]


def render(flags: list[Flag]) -> str:
    gs = find_global_settings()
    src = gs.as_posix() if gs else "?"
    lines = [
        "# CLI compatibility (Freerouting 2.3.0 vs fr-cli)",
        "",
        "Generated by `bench/tools/cli_compat.py`. Do not edit by hand; the notes",
        "column is the only authored input (the `NOTES` dict in the script).",
        "",
        f"Java flags are those `GlobalSettings.applyCommandLineArguments` inspects (`{src}`),",
        "plus `--user_data_path` (handled in `Freerouting.java` before that method).",
        "Ours are the match arms in `crates/fr-cli/src/main.rs`.",
        "",
        "Status: **兼容** same flag, same job; **部分** same flag with a documented",
        "divergence; **不支持** Java has it, we do not (or we reused the spelling);",
        "**我方独有** we have it, 2.3.0 does not.",
        "",
        "| flag | status | Java 2.3.0 | fr-cli | notes |",
        "|---|---|---|---|---|",
    ]
    for f in flags:
        note = f.note.replace("|", "\\|")
        j = f.java_meaning.replace("|", "\\|")
        o = f.ours_meaning.replace("|", "\\|")
        lines.append(f"| `{f.name}` | {f.status} | {j} | {o} | {note} |")
    lines += [
        "",
        "## `-us` compatibility",
        "",
        "Upstream `-us` selects the optimizer's board-update strategy (`greedy|global|hybrid`).",
        "`fr-cli` accepts strategy names as well as numeric worker thread counts (`-us <N>` as a legacy",
        "alias for `--threads <N>` / `-mt <N>`).",
        "",
    ]
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    flags = collect()
    text = render(flags)
    if args.check:
        current = DOC_PATH.read_text(encoding="utf-8") if DOC_PATH.is_file() else ""
        if current != text:
            print("docs/CLI.md is stale; run cli_compat.py --write", file=sys.stderr)
            return 1
        print("ok: docs/CLI.md is up to date")
        return 0
    if args.write:
        DOC_PATH.write_text(text, encoding="utf-8")
        print(f"wrote {DOC_PATH} ({len(flags)} flags)")
        return 0
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
