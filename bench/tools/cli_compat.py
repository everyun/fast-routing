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
    "-de": "Java accepts several files and dispatches by extension (dsn/json/ses/rules) and `a.dsn+b.rules`. We take one `.dsn`. Alias `-di` is our synonym for `-de`, not Java's GUI directory.",
    "-di": "Java: GUI input directory. We treat `-di` as `-de`.",
    "-do": "Output SES path. Default `<input>.ses` on both sides for a routing run.",
    "-dr": "Rules sidecar. Unsupported.",
    "-drc": "Java DRC-only mode (disables the router, writes JSON). Our `--drc-report` emits the same-shaped JSON without forcing the other Java side effects.",
    "-mp": "Pass cap. Java clamps to `[0,9999]` (0 = unlimited) and a *negative* value is a latent uncapped run. We reject negatives (PARITY-DIVERGENCES).",
    "-mt": "Java optimizer thread count, clamped `[0,1024]`. We do not take `-mt`; `--threads` is a different budget (optimizer T1 + referee T2).",
    "-oit": "Optimizer improvement threshold in **percent**. Unsupported.",
    "-us": "Java: optimizer board-update strategy (`greedy|global|hybrid`). **Collision:** we parse `-us` as `--threads`. Java meaning is unsupported.",
    "-is": "Optimizer item-selection strategy. Unsupported.",
    "-hr": "Optimizer hybrid ratio. Unsupported.",
    "-inc": "Ignore net classes. GUI-only effect upstream; we do not take the flag (intentionally dead).",
    "-l": "Locale. Unsupported (headless, English messages).",
    "-dl": "Disable file log. Unsupported.",
    "-da": "Disable analytics. Unsupported (we have no analytics).",
    "-ll": "Console log level. Unsupported.",
    "-host": "Host tag for analytics. Unsupported.",
    "-dct": "GUI dialog confirmation timeout. Unsupported.",
    "-help": "Help. We also take `-h` / `--help`.",
    "-h": "Help. Java accepts it as an alias of `-help`.",
    "--help": "Help.",
    "--compare-boards": "Java board-compare utility. Unsupported.",
    "--user_data_path": "Java user-data directory. Required on this machine so the jar does not read `$TMPDIR/freerouting/freerouting.json`. We have no equivalent (no JSON settings file).",
    "--debug.*": "Java debug switches. Unsupported.",
    "--router.*": "Generic `ReflectionUtil` setter into `GlobalSettings`. We have named flags only.",
    "--<path>=<value>": "Generic setter for any `GlobalSettings` path (`--logging.*`, `--mcp_server.stdio=`, …). Unsupported.",
    "--threads": "Workers for optimizer candidate evaluation and the referee scan. Not the route pass. Default 1. Unique.",
    "--portfolio": "Race N route passes on board clones; keep the best referee score. Unique.",
    "--no-optimizer": "Skip the post-route optimizer (on by default, matching DefaultSettings). Unique spelling; Java uses `--router.optimizer.enabled=false`.",
    "--fanout": "Opt-in SMD escape pre-pass. Java default is on via settings; we default off in M2a and take an explicit flag. Unique spelling; Java uses `--router.fanout.enabled=`.",
    "--deterministic": "Effort-based virtual time instead of wall-clock budgets. Unique.",
    "--job-timeout": "Whole-job wall budget; on expiry write the SES and exit 4. Java has `router.job_timeout` as a settings string, not this flag.",
    "--import-only": "Import and judge; do not route. Unique.",
    "--golden-dump": "G1.1 model dump. Unique.",
    "--drc-report": "G1.2a `java -drc`-shaped JSON. Partial overlap with Java `-drc`.",
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
    "-mp",
    "--max-passes",
    "-us",
    "--threads",
    "--portfolio",
    "--fanout",
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
    "-h",
    "--help",
]


def classify_pair(name: str, java: set[str], ours: set[str]) -> Flag:
    flag = Flag(name=name, side="both" if name in java and name in ours else ("java" if name in java else "ours"))
    flag.note = NOTES.get(name, "")
    in_j, in_o = name in java, name in ours

    # Collisions / partials first.
    if name == "-us":
        flag.java_meaning = "optimizer board-update strategy"
        flag.ours_meaning = "--threads (worker count)"
        flag.status = "不支持"
        flag.note = NOTES["-us"]
        return flag
    if name == "-di":
        flag.java_meaning = "GUI input directory"
        flag.ours_meaning = "alias of -de"
        flag.status = "部分"
        return flag
    if name == "-de":
        flag.java_meaning = "one or more design files, dispatched by extension"
        flag.ours_meaning = "one .dsn"
        flag.status = "部分"
        return flag
    if name == "-mp":
        flag.java_meaning = "max passes, clamp [0,9999]; negative becomes uncapped"
        flag.ours_meaning = "max passes; negative rejected"
        flag.status = "部分"
        return flag
    if name == "-drc":
        flag.java_meaning = "DRC-only mode + JSON path"
        flag.ours_meaning = ""
        flag.status = "不支持"
        return flag
    if name == "--drc-report":
        flag.java_meaning = ""
        flag.ours_meaning = "G1.2a JSON (does not disable the router by itself)"
        flag.status = "部分"
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
    # Keep a stable, readable order: Java canonical, then remaining Java, then ours-only.
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
        "## `-us` collision",
        "",
        "Upstream `-us` selects the optimizer's board-update strategy. `fr-cli`",
        "`-us` is an alias of `--threads`. The two are not compatible. Use",
        "`--threads N` on our side; there is no flag for `board_update_strategy`.",
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
