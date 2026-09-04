#!/usr/bin/env python3
"""Generate `docs/SETTINGS.md` from the RouterSettings universe.

Universe (TASK-cleanup step 2), in this order:

1. Java `settings/RouterSettings.java` plus nested Layer / Fanout / Optimizer /
   Scoring settings (JSON `@SerializedName`, plus transient public fields).
2. DSN `(autoroute_settings …)` tokens (`AutorouteSettings.readScope`).
3. CLI flags that write a router knob (`GlobalSettings.applyCommandLineArguments`
   and `sources/CliSettings`).

Status is derived from this tree plus a scan of our consumers. The NOTES dict
is the only handwritten column.

    python3 bench/tools/settings_scoreboard.py --write
    python3 -m unittest discover -s bench/tools/tests -p 'test_settings_scoreboard.py'
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DOC_PATH = REPO / "docs" / "SETTINGS.md"
PARITY = REPO / "docs" / "PARITY-DIVERGENCES.md"
ARCHITECTURE = REPO / "docs" / "ARCHITECTURE.md"
RUST_SETTINGS = REPO / "crates" / "fr-rules" / "src" / "router_settings.rs"
RUST_APPLY = REPO / "crates" / "fr-io" / "src" / "specctra" / "autoroute_settings.rs"
RUST_READ = REPO / "crates" / "fr-io" / "src" / "specctra" / "read.rs"
RUST_CLI = REPO / "crates" / "fr-cli" / "src" / "main.rs"

JAVA_ROOTS = [
    Path("/tmp/fr-java-release/src"),
    Path(os.path.expanduser("~/fr-ref/fr-java-release/src")),
]

CONSUMER_DIRS = [
    REPO / "crates" / "fr-router" / "src",
    REPO / "crates" / "fr-core" / "src",
    REPO / "crates" / "fr-io" / "src",
    REPO / "crates" / "fr-cli" / "src",
    REPO / "crates" / "fr-check" / "src",
]

# Nested Java class → JSON prefix under `router.`
NESTED = {
    "LayerSettings.java": "layers[]",
    "FanoutSettings.java": "fanout",
    "OptimizerSettings.java": "optimizer",
    "ScoringSettings.java": "scoring",
}

SKIP_FIELDS = {
    "boardSpecificTraceCostsApplied",
    "pcs",
}

# Handwritten notes only. Status is never taken from here.
NOTES: dict[str, str] = {
    "router.enabled": "Gates the batch loop (`run_router`). DSN `(autoroute on|off)` writes it.",
    "router.algorithm": "Headless 2.3.0 hard-codes `BatchAutorouter`; the selector is GUI-only.",
    "router.copper_to_edge_clearance_um": "Applied at import (`fr_io::specctra::build`), default 500 µm.",
    "router.hole_clearance_um": "Applied at import (`fr_io::specctra::build`), default 0.",
    "router.neck_width_um": "Failed-connection retry clamp. Stored; maze does not yet retry at this width.",
    "router.strict_drc": "Would rip a new connection that introduced violations. Stored, not wired.",
    "router.job_timeout": "Java string (`12:00:00` / `5m`). Our wall budget is CLI `--job-timeout`, a different mechanism.",
    "router.max_passes": "CLI `-mp`. We reject a negative value (PARITY-DIVERGENCES); 0 is unlimited upstream.",
    "router.max_items": "Hard cap on `routed + failed`. Read by `BatchRouter`.",
    "router.layers[]": "Sized from the board. Children below.",
    "router.save_intermediate_stages": "Per-pass board dump. Transient, GUI/debug. Not wired.",
    "router.ignore_net_classes": "CLI `-inc`. GUI-only upstream; headless no-op. Replicated as intentionally dead (PARITY-DIVERGENCES).",
    "router.trace_pull_tight_accuracy": "AutorouteControl hard-codes 500 and does not read the setting. We match (`PULL_TIGHT_ACCURACY`).",
    "router.allowed_via_types": "JSON name is a misnomer; it is boolean `vias_allowed`.",
    "router.automatic_neckdown": "Feeds `AutorouteControl.with_neckdown`.",
    "router.max_threads": "Java setter mirrors into `optimizer.max_threads`. Our `--threads` is a separate worker budget (optimizer T1 + referee T2), not this field.",
    "router.layers[].routable": "DSN `(layer_rule L (active …))`. Non-signal layers forced off in `apply_board_specific_optimizations`.",
    "router.layers[].preferred_direction_horizontal": "DSN `(preferred_direction …)`. Seeded from board aspect when unset.",
    "router.layers[].bend_cost": "v2 only. `AutorouteControl.bend_costs[i]`.",
    "router.fanout.enabled": "Two null policies: `run_fanout` vs `is_fanout_enabled`. Headless gates on the latter. Our CLI `--fanout` is the opt-in.",
    "router.fanout.max_passes": "Fanout pass cap. `FanoutConfig::from_settings`.",
    "router.fanout.max_items": "Escape-attempt cap. `FanoutConfig::from_settings`.",
    "router.fanout.max_milliseconds_per_pin": "Per-pin time budget. Stored; fanout does not yet spend it.",
    "router.fanout.ripup_allowed": "Fanout may rip traces. `FanoutConfig::from_settings`.",
    "router.fanout.min_escape_length_mm": "Via distance floor, mm. Scaled in `from_settings`.",
    "router.fanout.max_escape_length_mm": "Search radius, mm. Scaled in `from_settings`.",
    "router.fanout.start_via_diameter_mm": "Via-in-pad diameter. Stored, not yet consumed by fanout.",
    "router.fanout.end_via_diameter_mm": "Landing via diameter. Stored, not yet consumed by fanout.",
    "router.fanout.pin_sorting_order": "`outer_first` / `inner_first` / `unsorted`. `FanoutConfig::from_settings`.",
    "router.fanout.fallback_to_board_vias": "`FanoutConfig::from_settings`.",
    "router.fanout.timeout": "Stage timeout string. Stored, not polled.",
    "router.optimizer.enabled": "`run_optimizer()` is fail-closed on null; DefaultSettings sets true. `--no-optimizer` clears it.",
    "router.optimizer.algorithm": "Selector token. Headless always constructs `BatchOptimizer`.",
    "router.optimizer.max_passes": "Optimizer sweep cap. Read by `BatchOptimizer`.",
    "router.optimizer.max_items": "Item cap. Read by `BatchOptimizer`.",
    "router.optimizer.max_threads": "Java parallel optimizer. Our `--threads` drives T1 clone-eval, not this field directly.",
    "router.optimizer.improvement_threshold": "Early-exit fraction. Read by `BatchOptimizer`. CLI `-oit` is percent.",
    "router.optimizer.max_consecutive_failures": "Pass abort. Read by `BatchOptimizer`.",
    "router.optimizer.additional_ripup_cost_factor_at_start": "Ripup cost multiplier at optimizer start.",
    "router.optimizer.trace_ripup_cost_factor": "Discount when the ripped item is a trace.",
    "router.optimizer.max_autoroute_passes": "Inner maze passes per optimized item.",
    "router.optimizer.board_update_strategy": "CLI `-us greedy|global|hybrid`. We reused `-us` for `--threads`. Strategy enum is stored, not wired.",
    "router.optimizer.hybrid_ratio": "CLI `-hr`. Stored, not wired.",
    "router.optimizer.item_selection_strategy": "CLI `-is seq|rand|prio`. Stored, not wired.",
    "router.optimizer.timeout": "Stage timeout string. Stored, not polled.",
    "router.scoring.preferred_direction_trace_cost": "Per-layer, derived by `apply_board_specific_optimizations`. DSN values do not survive the merge (copyFields / recompute).",
    "router.scoring.undesired_direction_trace_cost": "Same as preferred: derived, DSN numbers discarded.",
    "router.scoring.default_preferred_direction_trace_cost": "Seed for the preferred array and the score length term.",
    "router.scoring.default_undesired_direction_trace_cost": "Seed before the aspect-ratio add-on.",
    "router.scoring.via_costs": "Maze via cost and score term. DSN `(via_costs n)`.",
    "router.scoring.plane_via_costs": "Via on a plane. DSN `(plane_via_costs n)`.",
    "router.scoring.start_ripup_costs": "Pass n rips at `start_ripup_costs * n`. DSN `(start_ripup_costs n)`.",
    "router.scoring.unrouted_net_penalty": "Score + normalizer. `fr_router::batch::scoring` / `fr_check::score`.",
    "router.scoring.clearance_violation_penalty": "Score term. Same consumers.",
    "router.scoring.bend_penalty": "Score term (not the maze).",
    "router.scoring.default_bend_cost": "Fallback for `layers[i].bend_cost`.",
    "dsn.fanout": "Parsed and discarded upstream (`AutorouteSettings.java`). We do the same.",
    "dsn.autoroute": "Writes `router.enabled`. Applied.",
    "dsn.postroute": "Writes `router.optimizer.enabled`. Applied.",
    "dsn.vias": "Writes `router.vias_allowed`. Applied.",
    "dsn.via_costs": "Writes `scoring.via_costs`, clamped >= 1. Applied.",
    "dsn.plane_via_costs": "Writes `scoring.plane_via_costs`. Applied.",
    "dsn.start_ripup_costs": "Writes `scoring.start_ripup_costs`. Applied.",
    "dsn.start_pass_no": "Keyword exists; `readScope` has no branch. Discarded upstream and here.",
    "dsn.layer_rule.active": "Writes `layers[i].routable`. Applied.",
    "dsn.layer_rule.preferred_direction": "Writes `layers[i].preferred_direction_horizontal`. Applied.",
    "dsn.layer_rule.preferred_direction_trace_costs": "Parsed; not applied. `copyFields` cannot keep a primitive array across the merge, and `applyBoardSpecificOptimizations` recomputes both arrays.",
    "dsn.layer_rule.against_preferred_direction_trace_costs": "Same as preferred_direction_trace_costs.",
    "cli.-mp": "Maps to `router.max_passes`. We reject negatives (PARITY-DIVERGENCES).",
    "cli.-mt": "Maps to `router.optimizer.max_threads` (clamped 0..=1024). We do not take `-mt`.",
    "cli.-oit": "Percent → `optimizer.improvement_threshold / 100`. We do not take `-oit`.",
    "cli.-us": "Java: optimizer board-update strategy. We reused the flag for `--threads`. Collision; Java meaning unsupported.",
    "cli.-is": "Optimizer item-selection strategy. Unsupported.",
    "cli.-hr": "Optimizer hybrid ratio. Unsupported.",
    "cli.-inc": "Writes `ignore_net_classes`. Intentionally dead headless, both sides.",
    "cli.--router.*": "Generic `ReflectionUtil.setFieldValue` into `GlobalSettings`. We have no reflection setter.",
}


@dataclass
class Knob:
    key: str
    source: str
    java_field: str = ""
    rust_field: str = ""
    rust_loc: str = ""
    default: str = ""
    status: str = ""
    evidence: str = ""
    note: str = ""
    needles: list[str] = field(default_factory=list)


def find_java_settings_dir() -> Path | None:
    for root in JAVA_ROOTS:
        candidate = root / "main/java/app/freerouting/settings"
        if (candidate / "RouterSettings.java").is_file():
            return candidate
        # Flat `src/` as the task wrote it.
        for path in root.rglob("RouterSettings.java"):
            return path.parent
    return None


def camel_to_snake(name: str) -> str:
    name = name.replace("[]", "")
    name = re.sub(r"String$", "", name)
    name = re.sub(r"(.)([A-Z][a-z]+)", r"\1_\2", name)
    name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name)
    return name.lower()


# Class-body fields are indented two spaces. Method locals are four or more,
# and must not enter the universe.
_FIELD_RE = re.compile(
    r"@SerializedName\(\"(?P<json>[^\"]+)\"\)\s+"
    r"(?P<mods>(?:public|protected|private|transient|static|final| )+)"
    r"(?P<ty>[A-Za-z0-9_.<>, \[\]]+?) +(?P<field>[A-Za-z_][A-Za-z0-9_]*) *[=;]",
)
_BARE_FIELD_RE = re.compile(
    r"^  (?P<mods>(?:public|protected)(?: transient)?(?: final)? )"
    r"(?P<ty>[A-Za-z0-9_.<>, \[\]]+?) +(?P<field>[A-Za-z_][A-Za-z0-9_]*) *[=;]",
    re.M,
)
_JAVA_KEYWORDS = {
    "true",
    "false",
    "return",
    "that",
    "copy",
    "result",
    "class",
    "new",
    "this",
    "null",
}


def parse_java_class(text: str) -> list[tuple[str, str, str]]:
    """Return (json_key, java_field, type) for public/protected instance fields."""
    found: list[tuple[str, str, str]] = []
    seen: set[str] = set()
    for match in _FIELD_RE.finditer(text):
        if "static" in match.group("mods"):
            continue
        json_key = match.group("json")
        field = match.group("field")
        if field in SKIP_FIELDS or field in _JAVA_KEYWORDS or field.isupper():
            continue
        if "static" in match.group("mods"):
            continue
        ty = re.sub(r"\s+", " ", match.group("ty")).strip()
        found.append((json_key, field, ty))
        seen.add(field)
    for match in _BARE_FIELD_RE.finditer(text):
        field = match.group("field")
        if field in seen or field in SKIP_FIELDS or field in _JAVA_KEYWORDS or field.isupper():
            continue
        if "static" in match.group(0):
            continue
        ty = re.sub(r"\s+", " ", match.group("ty")).strip()
        json_key = camel_to_snake(field)
        found.append((json_key, field, ty))
        seen.add(field)
    return found


def parse_java_knobs(settings_dir: Path) -> list[Knob]:
    knobs: list[Knob] = []
    top = (settings_dir / "RouterSettings.java").read_text(encoding="utf-8", errors="replace")
    for json_key, java_field, ty in parse_java_class(top):
        if json_key in {"fanout", "optimizer", "scoring", "layers"}:
            continue
        knobs.append(
            Knob(
                key=f"router.{json_key}",
                source="RouterSettings.java",
                java_field=java_field,
                rust_field=_rust_field_for(json_key, java_field),
            )
        )
    for filename, prefix in NESTED.items():
        path = settings_dir / filename
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for json_key, java_field, ty in parse_java_class(text):
            knobs.append(
                Knob(
                    key=f"router.{prefix}.{json_key}",
                    source=filename,
                    java_field=java_field,
                    rust_field=_rust_field_for(json_key, java_field),
                )
            )
    return knobs


def _rust_field_for(json_key: str, java_field: str) -> str:
    # JSON names already match our Rust fields, with two known aliases.
    aliases = {
        "allowed_via_types": "vias_allowed",
        "job_timeout": "job_timeout",
        "timeout": "timeout",
    }
    return aliases.get(json_key, json_key if json_key == camel_to_snake(java_field) or "_" in json_key else camel_to_snake(java_field))


_RUST_STRUCT_RE = re.compile(
    r"pub struct (?P<name>\w+) \{(?P<body>.*?)^\}",
    re.S | re.M,
)
_RUST_FIELD_RE = re.compile(r"pub (?P<field>[A-Za-z_][A-Za-z0-9_]*):")


def parse_rust_structs(text: str) -> dict[str, list[str]]:
    out: dict[str, list[str]] = {}
    for match in _RUST_STRUCT_RE.finditer(text):
        fields = _RUST_FIELD_RE.findall(match.group("body"))
        out[match.group("name")] = fields
    return out


def rust_field_line(text: str, field: str, struct: str | None = None) -> int | None:
    body = text
    if struct:
        match = re.search(
            rf"pub struct {re.escape(struct)} \{{(?P<body>.*?)^\}}",
            text,
            re.S | re.M,
        )
        if not match:
            return None
        body = match.group("body")
        offset = match.start("body")
    else:
        offset = 0
    pattern = re.compile(rf"^\s*pub {re.escape(field)}:", re.M)
    found = pattern.search(body)
    if not found:
        return None
    return text[: offset + found.start()].count("\n") + 1


def rust_struct_for(key: str) -> str:
    if key.startswith("router.fanout."):
        return "FanoutSettings"
    if key.startswith("router.optimizer."):
        return "OptimizerSettings"
    if key.startswith("router.scoring."):
        return "ScoringSettings"
    if key.startswith("router.layers[]."):
        return "LayerSettings"
    return "RouterSettings"


def dsn_knobs() -> list[Knob]:
    """Tokens `AutorouteSettings.readScope` actually looks at, plus the discarded start_pass_no."""
    rows = [
        ("dsn.fanout", "FANOUT", "discarded"),
        ("dsn.autoroute", "AUTOROUTE", "run_router"),
        ("dsn.postroute", "POSTROUTE", "run_optimizer"),
        ("dsn.vias", "VIAS", "vias_allowed"),
        ("dsn.via_costs", "VIA_COSTS", "via_costs"),
        ("dsn.plane_via_costs", "PLANE_VIA_COSTS", "plane_via_costs"),
        ("dsn.start_ripup_costs", "START_RIPUP_COSTS", "start_ripup_costs"),
        ("dsn.start_pass_no", "START_PASS_NO", "discarded"),
        ("dsn.layer_rule.active", "ACTIVE", "layer.routable"),
        ("dsn.layer_rule.preferred_direction", "PREFERRED_DIRECTION", "layer.preferred_direction"),
        (
            "dsn.layer_rule.preferred_direction_trace_costs",
            "PREFERRED_DIRECTION_TRACE_COSTS",
            "discarded-after-parse",
        ),
        (
            "dsn.layer_rule.against_preferred_direction_trace_costs",
            "AGAINST_PREFERRED_DIRECTION_TRACE_COSTS",
            "discarded-after-parse",
        ),
    ]
    knobs = []
    for key, java_kw, rust_field in rows:
        knobs.append(
            Knob(
                key=key,
                source="AutorouteSettings.java / DSN",
                java_field=java_kw,
                rust_field=rust_field,
            )
        )
    return knobs


def cli_setting_knobs() -> list[Knob]:
    return [
        Knob(key="cli.-mp", source="CLI", java_field="-mp", rust_field="max_passes"),
        Knob(key="cli.-mt", source="CLI", java_field="-mt", rust_field="optimizer.max_threads"),
        Knob(key="cli.-oit", source="CLI", java_field="-oit", rust_field="improvement_threshold"),
        Knob(key="cli.-us", source="CLI", java_field="-us", rust_field="board_update_strategy"),
        Knob(key="cli.-is", source="CLI", java_field="-is", rust_field="item_selection_strategy"),
        Knob(key="cli.-hr", source="CLI", java_field="-hr", rust_field="hybrid_ratio"),
        Knob(key="cli.-inc", source="CLI", java_field="-inc", rust_field="ignore_net_classes"),
        Knob(key="cli.--router.*", source="CLI", java_field="--router.*=", rust_field=""),
    ]


def _iter_rs(root: Path):
    for path in root.rglob("*.rs"):
        rel = str(path)
        if "/tests/" in rel or path.name.endswith("_test.rs"):
            continue
        yield path


def grep_needles(needles: list[str]) -> list[str]:
    hits: list[str] = []
    if not needles:
        return hits
    for root in CONSUMER_DIRS:
        if not root.is_dir():
            continue
        for path in _iter_rs(root):
            # The settings struct itself is not a consumer.
            if path.resolve() == RUST_SETTINGS.resolve():
                continue
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            if any(n in text for n in needles):
                rel = path.relative_to(REPO).as_posix()
                hits.append(rel)
    return sorted(set(hits))


def _needles_for(knob: Knob) -> list[str]:
    field = knob.rust_field
    if not field or field in {"discarded", "discarded-after-parse"}:
        return []
    if knob.key.startswith("dsn."):
        mapping = {
            "run_router": ["dsn.run_router", "run_router", "settings.enabled"],
            "run_optimizer": ["dsn.run_optimizer", "run_optimizer", "optimizer.enabled"],
            "vias_allowed": ["dsn.vias_allowed", "vias_allowed"],
            "via_costs": ["set_via_costs", "dsn.via_costs"],
            "plane_via_costs": ["set_plane_via_costs", "dsn.plane_via_costs"],
            "start_ripup_costs": ["set_start_ripup_costs", "dsn.start_ripup_costs"],
            "layer.routable": ["set_layer_active", "layer.active"],
            "layer.preferred_direction": [
                "set_preferred_direction_is_horizontal",
                "preferred_direction_is_horizontal",
            ],
        }
        return mapping.get(field, [field])
    if knob.key.startswith("cli."):
        return [knob.java_field] if knob.java_field else []
    # RouterSettings field: require a dotted access or a dedicated getter.
    dotted = field if "." in field else None
    out = []
    if knob.key.startswith("router.fanout."):
        out.append(f"fanout.{field}")
    elif knob.key.startswith("router.optimizer."):
        out.append(f"optimizer.{field}")
    elif knob.key.startswith("router.scoring."):
        out.append(f"scoring.{field}")
        out.append(f"scoring.{field}(")
    elif knob.key.startswith("router.layers[]."):
        out.append(f"layer.{field}")
        if field == "routable":
            out.extend(["layer_active", "set_layer_active"])
        if field == "preferred_direction_horizontal":
            out.extend(
                [
                    "preferred_direction_is_horizontal",
                    "set_preferred_direction_is_horizontal",
                ]
            )
        if field == "bend_cost":
            out.append("bend_cost(")
    else:
        out.append(f"settings.{field}")
        getters = {
            "enabled": ["run_router("],
            "vias_allowed": ["settings.vias_allowed"],
            "automatic_neckdown": ["settings.automatic_neckdown"],
            "max_passes": ["settings.max_passes"],
            "max_items": ["settings.max_items"],
            "max_threads": ["settings.max_threads"],
            "copper_to_edge_clearance_um": ["copper_to_edge_clearance_um"],
            "hole_clearance_um": ["hole_clearance_um"],
        }
        out.extend(getters.get(field, []))
    # Dedup while preserving order.
    seen: set[str] = set()
    uniq = []
    for n in out:
        if n and n not in seen:
            seen.add(n)
            uniq.append(n)
    return uniq


def _docs_text() -> str:
    chunks = []
    for path in (PARITY, ARCHITECTURE):
        if path.is_file():
            chunks.append(path.read_text(encoding="utf-8", errors="replace"))
    return "\n".join(chunks)


DEAD_HINTS = (
    "intentionally dead",
    "intentionally-dead",
    "no-op headless",
    "gui-only",
    "dead in headless",
    "ignoring the setting",
)


def is_documented_dead(knob: Knob, docs: str) -> bool:
    names = [knob.key]
    tail = knob.key.split(".")[-1]
    if len(tail) >= 6:
        names.append(tail)
    if knob.java_field and len(knob.java_field) >= 6:
        names.append(knob.java_field)
    for name in names:
        for match in re.finditer(re.escape(name), docs, re.I):
            window = docs[max(0, match.start() - 160) : match.end() + 160].lower()
            if any(hint in window for hint in DEAD_HINTS):
                return True
    return False


def classify(knob: Knob, docs: str, rust_text: str) -> None:
    knob.note = NOTES.get(knob.key, "")
    skip_line = knob.rust_field in {
        "",
        "discarded",
        "discarded-after-parse",
        "run_router",
        "run_optimizer",
        "layer.routable",
        "layer.preferred_direction",
    }
    line = None
    if not skip_line and knob.rust_field:
        line = rust_field_line(rust_text, knob.rust_field, rust_struct_for(knob.key))
    if line:
        knob.rust_loc = f"crates/fr-rules/src/router_settings.rs:{line}"

    if knob.key.startswith("dsn."):
        apply_text = RUST_APPLY.read_text(encoding="utf-8", errors="replace") if RUST_APPLY.is_file() else ""
        read_text = RUST_READ.read_text(encoding="utf-8", errors="replace") if RUST_READ.is_file() else ""
        if knob.rust_field in {"discarded", "discarded-after-parse"}:
            knob.status = "intentionally-dead"
            knob.evidence = "parsed (or skipped) to match upstream; not applied"
            if "start_pass_no" in knob.key:
                knob.rust_loc = "crates/fr-io/src/specctra/read.rs (StartPassNo branch discards)"
            elif "fanout" in knob.key:
                knob.rust_loc = "crates/fr-io/src/specctra/read.rs (Fanout on/off discarded)"
            else:
                knob.rust_loc = "crates/fr-io/src/specctra/autoroute_settings.rs (not applied)"
            return
        applied = False
        if knob.rust_field == "run_router" and "dsn.run_router" in apply_text or "run_router.unwrap_or" in apply_text:
            applied = True
        if knob.rust_field == "run_optimizer" and "run_optimizer" in apply_text:
            applied = True
        if knob.rust_field == "vias_allowed" and "vias_allowed" in apply_text:
            applied = True
        if knob.rust_field == "via_costs" and "set_via_costs" in apply_text:
            applied = True
        if knob.rust_field == "plane_via_costs" and "set_plane_via_costs" in apply_text:
            applied = True
        if knob.rust_field == "start_ripup_costs" and "set_start_ripup_costs" in apply_text:
            applied = True
        if knob.rust_field == "layer.routable" and "set_layer_active" in apply_text:
            applied = True
        if knob.rust_field == "layer.preferred_direction" and "set_preferred_direction_is_horizontal" in apply_text:
            applied = True
        knob.status = "implemented" if applied else "not-yet"
        knob.rust_loc = "crates/fr-io/src/specctra/autoroute_settings.rs"
        knob.evidence = "apply() writes the RouterSettings field" if applied else "reader keeps it; apply() does not"
        return

    if knob.key.startswith("cli."):
        cli_text = RUST_CLI.read_text(encoding="utf-8", errors="replace") if RUST_CLI.is_file() else ""
        flag = knob.java_field.split()[0] if knob.java_field else ""
        if knob.key == "cli.--router.*":
            knob.status = "rejected"
            knob.evidence = "PARITY: no reflection setter; named flags only"
            knob.rust_loc = "docs/PARITY-DIVERGENCES.md"
            return
        if knob.key == "cli.-inc":
            knob.status = "intentionally-dead"
            knob.evidence = "docs/PARITY-DIVERGENCES.md (headless no-op)"
            return
        if knob.key == "cli.-us":
            # Collision: we parse -us as --threads.
            if '"-us"' in cli_text or "| \"-us\"" in cli_text or '"-us" |' in cli_text:
                knob.status = "rejected"
                knob.evidence = "fr-cli reuses -us for --threads; Java meaning is board_update_strategy"
                knob.rust_loc = "crates/fr-cli/src/main.rs"
                return
        if knob.key == "cli.-mp":
            if "-mp" in cli_text:
                knob.status = "implemented"
                knob.evidence = "crates/fr-cli/src/main.rs (negative rejected; PARITY-DIVERGENCES)"
                knob.rust_loc = "crates/fr-cli/src/main.rs"
                return
        # Other short flags: present in Java, absent here.
        if flag and flag not in cli_text:
            knob.status = "not-yet"
            knob.evidence = f"{flag} is not a fr-cli flag"
            return
        knob.status = "not-yet"
        knob.evidence = "flag string present but does not write the Java knob"
        return

    # RouterSettings field.
    needles = _needles_for(knob)
    knob.needles = needles
    hits = grep_needles(needles)
    # copper/hole live on ImportOptions, not settings.X — still a real consumer.
    if knob.key == "router.copper_to_edge_clearance_um":
        hits = grep_needles(["copper_to_edge_clearance_um"])
    if knob.key == "router.hole_clearance_um":
        hits = grep_needles(["hole_clearance_um"])

    mentioned_dead = is_documented_dead(knob, docs)
    if "ignore_net_classes" in knob.key:
        mentioned_dead = True

    if mentioned_dead and not hits:
        knob.status = "intentionally-dead"
        knob.evidence = "docs/PARITY-DIVERGENCES.md / ARCHITECTURE.md; no routing consumer"
        if "ignore_net_classes" in knob.key:
            knob.evidence = "docs/PARITY-DIVERGENCES.md (headless no-op)"
        if "trace_pull_tight_accuracy" in knob.key:
            knob.evidence = "crates/fr-router/src/ctrl.rs (`PULL_TIGHT_ACCURACY = 500`); setting unread, matching upstream"
            knob.rust_loc = "crates/fr-router/src/ctrl.rs"
        return

    if hits:
        knob.status = "implemented"
        knob.evidence = ", ".join(hits[:4])
        if len(hits) > 4:
            knob.evidence += f" (+{len(hits) - 4})"
        return

    # Stored on RouterSettings (field exists in Rust) but nothing reads it
    # on a routing/import path.
    rust_has = bool(line) or (
        knob.rust_field and f"pub {knob.rust_field}:" in rust_text
    )
    if rust_has:
        knob.status = "not-yet"
        knob.evidence = "field exists on RouterSettings; no consumer outside fr-rules"
        return
    knob.status = "not-yet"
    knob.evidence = "present in Java 2.3.0; no matching Rust field consumer"


def render(knobs: list[Knob], java_dir: Path | None) -> str:
    src = (
        java_dir.as_posix()
        if java_dir
        else "(Java source missing — not generated from RouterSettings.java)"
    )
    lines = [
        "# RouterSettings scoreboard",
        "",
        "Generated by `bench/tools/settings_scoreboard.py`. Do not edit by hand;",
        "the notes column is the only authored input (the `NOTES` dict in the script).",
        "",
        f"Universe: Freerouting 2.3.0 `RouterSettings` at `{src}`, DSN",
        "`(autoroute_settings …)`, and CLI flags that write a router knob.",
        "",
        "Status values:",
        "",
        "- **implemented** — a consumer in `fr-router` / `fr-core` / `fr-io` / `fr-cli` reads it",
        "- **intentionally-dead** — stored or parsed to match upstream's no-op (e.g. `ignore_net_classes`)",
        "- **rejected** — we will not take this knob / flag meaning",
        "- **not-yet** — in the 2.3.0 surface, not wired on a routing path",
        "",
        "## `router.*`",
        "",
        "| knob | status | our location | evidence | notes |",
        "|---|---|---|---|---|",
    ]

    def row(k: Knob) -> str:
        note = k.note.replace("|", "\\|")
        loc = k.rust_loc.replace("|", "\\|")
        ev = k.evidence.replace("|", "\\|")
        return f"| `{k.key}` | {k.status} | `{loc}` | {ev} | {note} |"

    for k in knobs:
        if k.key.startswith("router."):
            lines.append(row(k))
    lines += [
        "",
        "## DSN `(autoroute_settings …)`",
        "",
        "| token | status | our location | evidence | notes |",
        "|---|---|---|---|---|",
    ]
    for k in knobs:
        if k.key.startswith("dsn."):
            lines.append(row(k))
    lines += [
        "",
        "## CLI flags that write a router knob",
        "",
        "The full CLI compatibility table is `docs/CLI.md`. This section is only",
        "the flags whose job is to set a `RouterSettings` field.",
        "",
        "| flag | status | our location | evidence | notes |",
        "|---|---|---|---|---|",
    ]
    for k in knobs:
        if k.key.startswith("cli."):
            lines.append(row(k))
    lines.append("")
    return "\n".join(lines)


def collect(java_dir: Path | None = None) -> list[Knob]:
    java_dir = java_dir or find_java_settings_dir()
    if java_dir is None:
        raise SystemExit(
            "Java RouterSettings.java not found under /tmp/fr-java-release/src "
            "or ~/fr-ref/fr-java-release/src"
        )
    rust_text = RUST_SETTINGS.read_text(encoding="utf-8", errors="replace")
    docs = _docs_text()
    knobs = parse_java_knobs(java_dir) + dsn_knobs() + cli_setting_knobs()
    # Stable unique by key (Java parse can theoretically duplicate).
    by_key: dict[str, Knob] = {}
    for k in knobs:
        by_key.setdefault(k.key, k)
    knobs = list(by_key.values())
    for k in knobs:
        classify(k, docs, rust_text)
    return knobs


def write_doc(path: Path | None = None) -> Path:
    java_dir = find_java_settings_dir()
    knobs = collect(java_dir)
    text = render(knobs, java_dir)
    out = path or DOC_PATH
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text, encoding="utf-8")
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help=f"write {DOC_PATH}")
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if docs/SETTINGS.md is stale",
    )
    args = parser.parse_args(argv)
    java_dir = find_java_settings_dir()
    knobs = collect(java_dir)
    text = render(knobs, java_dir)
    if args.check:
        current = DOC_PATH.read_text(encoding="utf-8") if DOC_PATH.is_file() else ""
        if current != text:
            print("docs/SETTINGS.md is stale; run settings_scoreboard.py --write", file=sys.stderr)
            return 1
        print("ok: docs/SETTINGS.md is up to date")
        return 0
    if args.write:
        DOC_PATH.write_text(text, encoding="utf-8")
        print(f"wrote {DOC_PATH} ({len(knobs)} knobs)")
        return 0
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
