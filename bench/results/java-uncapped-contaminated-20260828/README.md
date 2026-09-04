# Contaminated uncapped campaign (2026-08-28, pre-user_data_path isolation)

The 2 h-cap uncapped runs that produced the manifest's original
`fixtures[].uncapped` blocks and measurement-campaign.md §1. Copied verbatim
from the main tree's gitignored `bench/results/java-uncapped/` on 2026-09-05.

These ran with `$TMPDIR/freerouting/freerouting.json` leak active: fanout and
optimizer stages were disabled, so their walls are T_java(complete) for an
auto-routing-only pipeline. The hermetic re-run (2026-09-05, fanout+optimizer
on) lives in `bench/results/java-uncapped/`; see the "hermetic re-run" section
of bench/analysis/measurement-campaign.md for the per-board comparison.

`<stem>.log` = jar stdout + `/usr/bin/time -l` footer, 7200 s cap, W=4.
