# java_dump — the G1.0 oracle dump

`java -de f.dsn -do f.import.dsn` does **not** produce a DSN on the release jar:
the v2.3.0 headless pipeline (`HeadlessBoardManager`) only ever writes SES, so
the ROADMAP G1.0 recipe is un-runnable as written. `DumpDsn` is the working
replacement: it loads the DSN through the release jar's own `DsnReader`,
applies `reduce_nets_of_route_items` (the same resolved-model step the router
runs on import), and re-serialises with `DsnWriter` — i.e. Java's resolved
internal board model as a DSN, which is what `bench/golden/dumps/` stores and
what `golden_compare.py --java-dump` compares against.

```sh
bench/tools/java_dump/compile.sh            # once, against the reference jar
java -Djava.awt.headless=true \
  -cp /tmp/frbase/freerouting-2.3.0.jar:bench/tools/java_dump \
  DumpDsn in.dsn out.import.dsn
```

No `--user_data_path` is needed: this never enters the headless job pipeline,
so no settings file is read. Verified byte-identical against four checked-in
goldens (`Issue026-J2_reference`, `Issue039-bug-design`,
`Issue190-processor.Z80`, `Issue214-freerouting`).

Why not `-do`: the recipe `java -de f.dsn -do f.import.dsn` asks
`HeadlessBoardManager` to write a DSN, and the v2.3.0 headless manager only
implements SES output — the command silently writes a session. `DumpDsn`
reproduces the intended pipeline faithfully: `loadFromSpecctraDsn` (DsnReader
plus the `board_edge` / `hole_edge` clearance-class synthesis and
`reduce_nets_of_route_items`) then `DsnWriter`, with the pcb named after the
output stem exactly as `-do f.import.dsn` named it. The two clearance
overrides only run when the job's settings carry the CLI defaults, which a
bare `new RouterSettings()` does not — `DumpDsn` arms
`DefaultSettings.DEFAULT_COPPER_TO_EDGE_CLEARANCE_UM` /
`DEFAULT_HOLE_CLEARANCE_UM` explicitly.
