#!/usr/bin/env python3
"""Room-decomposition statistics, for our trace and the lab jar's alike.

The two sides emit the same two records — see
`crates/fr-router/src/rooms/trace.rs` and `AutorouteEngine.java:394-500`:

    ours:  FRROOM added, net=76, layer=0, bounds=[(a,b)..(c,d)], area=…, corners=…
    java:  TRACE  COMPLETE_ROOM added, net=76, layer=0, bounds=[(a,b)..(c,d)]

`bounds` is the only field both sides carry, so every statistic here is derived
from the bounding box: it is the common denominator, and the question being
asked — *how finely is free space cut up?* — is answered by it.

Usage:
    room_stats.py summary <log>...          per-log totals
    room_stats.py nets    <log> [--top N]   per-net rooms / median bbox area
    room_stats.py compare <ours> <java>     the side-by-side table

A *sliver* is a room whose bounding box is thinner than `--sliver` (default
1000 board units) in either axis: those are the tiles that cost a corner in a
located chain without buying any room to move in.

Stdlib only, like the rest of `bench/tools`.
"""

from __future__ import annotations

import argparse
import re
import statistics
import sys
from collections import defaultdict
from dataclasses import dataclass, field

ADDED_RE = re.compile(
    r"(?:FRROOM|COMPLETE_ROOM) added, net=(-?\d+), layer=(-?\d+), "
    r"bounds=\((-?\d+),(-?\d+)\)\.\.\((-?\d+),(-?\d+)\)"
)
# Both sides print the box as `[(lx,ly)..(rx,uy)]`; the brackets are stripped
# before the match so one pattern serves either bracket style.
INPUT_RE = re.compile(r"(?:FRROOM|COMPLETE_ROOM) input, net=(-?\d+), layer=(-?\d+)")

# Our located-chain record, and the Java run of per-segment records it is
# reconstructed from (`InsertFoundConnectionAlgo.java:161`). Java emits one line
# per segment with a per-trace `i` that restarts at 1, so a maximal run of
# ascending `i` is one located trace and its last `i` is the corner count minus
# one (`i` indexes segments, not corners).
LOCATED_RE = re.compile(r"FRROOM located, net=(-?\d+), hops=(\d+), corners=(\d+)")
SEGMENT_RE = re.compile(
    r"compare_trace_insert_segment_raw net=(-?\d+), layer=(-?\d+), i=(\d+)"
)

DEFAULT_SLIVER = 1000


@dataclass
class Room:
    net: int
    layer: int
    width: int
    height: int

    @property
    def area(self) -> int:
        return self.width * self.height

    def is_sliver(self, threshold: int) -> bool:
        return self.width < threshold or self.height < threshold


@dataclass
class Log:
    path: str
    rooms: list[Room] = field(default_factory=list)
    inputs: int = 0
    #: corner count of each located/inserted trace, in walk order
    chains: list[int] = field(default_factory=list)

    @property
    def nets(self) -> set[int]:
        return {room.net for room in self.rooms}


def parse(path: str) -> Log:
    log = Log(path=path)
    # Java's segment records carry no end marker; a run ends when `i` stops
    # ascending or the net changes.
    run_last: int | None = None
    run_net: int | None = None

    def close_run() -> None:
        nonlocal run_last, run_net
        if run_last is not None:
            log.chains.append(run_last + 1)
        run_last, run_net = None, None

    with open(path, "r", errors="replace") as handle:
        for line in handle:
            if "added, net=" in line:
                match = ADDED_RE.search(line.replace("[", "").replace("]", ""))
                if match:
                    net, layer, lx, ly, rx, uy = (int(g) for g in match.groups())
                    log.rooms.append(Room(net, layer, rx - lx, uy - ly))
                    continue
            if "input, net=" in line and INPUT_RE.search(line):
                log.inputs += 1
                continue
            match = LOCATED_RE.search(line)
            if match:
                log.chains.append(int(match.group(3)))
                continue
            match = SEGMENT_RE.search(line)
            if match:
                net, _, index = (int(g) for g in match.groups())
                if run_last is None or net != run_net or index <= run_last:
                    close_run()
                run_last, run_net = index, net
    close_run()
    return log


def by_net(log: Log) -> dict[int, list[Room]]:
    out: dict[int, list[Room]] = defaultdict(list)
    for room in log.rooms:
        out[room.net].append(room)
    return out


def summarise(log: Log, sliver: int) -> dict[str, float]:
    rooms = log.rooms
    out: dict[str, float] = {"rooms": len(rooms), "inputs": log.inputs}
    if rooms:
        areas = sorted(room.area for room in rooms)
        nets = by_net(log)
        out.update(
            {
                "nets": len(nets),
                "rooms_per_net": len(rooms) / len(nets),
                "area_p50": statistics.median(areas),
                "area_p10": areas[len(areas) // 10],
                "area_mean": sum(areas) / len(areas),
                "sliver_frac": sum(r.is_sliver(sliver) for r in rooms) / len(rooms),
            }
        )
    if log.chains:
        chains = sorted(log.chains)
        out.update(
            {
                "chains": len(chains),
                "corners_p50": statistics.median(chains),
                "corners_p90": chains[int(0.9 * (len(chains) - 1))],
                "corners_mean": sum(chains) / len(chains),
                "corners_max": max(chains),
            }
        )
    return out


def fmt(value: float) -> str:
    if isinstance(value, int) or value == int(value):
        return f"{int(value):,}"
    return f"{value:,.3f}"


def cmd_summary(args: argparse.Namespace) -> int:
    for path in args.logs:
        stats = summarise(parse(path), args.sliver)
        print(path)
        for key, value in stats.items():
            print(f"  {key:<14} {fmt(value)}")
    return 0


def cmd_nets(args: argparse.Namespace) -> int:
    log = parse(args.log)
    rows = []
    for net, rooms in by_net(log).items():
        areas = sorted(room.area for room in rooms)
        rows.append(
            (
                len(rooms),
                net,
                statistics.median(areas),
                sum(r.is_sliver(args.sliver) for r in rooms),
            )
        )
    rows.sort(reverse=True)
    print(f"{'rooms':>8} {'net':>6} {'area_p50':>16} {'slivers':>8}")
    for count, net, area_p50, slivers in rows[: args.top]:
        print(f"{count:>8,} {net:>6} {area_p50:>16,.0f} {slivers:>8,}")
    return 0


def cmd_compare(args: argparse.Namespace) -> int:
    ours, java = parse(args.ours), parse(args.java)
    left, right = summarise(ours, args.sliver), summarise(java, args.sliver)
    keys = [k for k in left if k in right]
    print(f"{'metric':<14} {'ours':>18} {'java':>18} {'ratio':>8}")
    for key in keys:
        a, b = left[key], right[key]
        ratio = f"{a / b:.2f}x" if b else "-"
        print(f"{key:<14} {fmt(a):>18} {fmt(b):>18} {ratio:>8}")

    shared = sorted(ours.nets & java.nets)
    if shared:
        ours_net, java_net = by_net(ours), by_net(java)
        ratios = [len(ours_net[n]) / len(java_net[n]) for n in shared]
        print(
            f"\nshared nets: {len(shared)}  "
            f"rooms/net ratio p50={statistics.median(ratios):.2f}x "
            f"max={max(ratios):.2f}x"
        )
        worst = sorted(shared, key=lambda n: -len(ours_net[n]) / len(java_net[n]))
        print(f"{'net':>6} {'ours':>8} {'java':>8} {'ratio':>8}")
        for net in worst[: args.top]:
            o, j = len(ours_net[net]), len(java_net[net])
            print(f"{net:>6} {o:>8,} {j:>8,} {o / j:>7.2f}x")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sliver", type=int, default=DEFAULT_SLIVER)
    parser.add_argument("--top", type=int, default=15)
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("summary")
    p.add_argument("logs", nargs="+")
    p.set_defaults(func=cmd_summary)

    p = sub.add_parser("nets")
    p.add_argument("log")
    p.set_defaults(func=cmd_nets)

    p = sub.add_parser("compare")
    p.add_argument("ours")
    p.add_argument("java")
    p.set_defaults(func=cmd_compare)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
