# Contaminated Java reference (pre-user_data_path isolation)

Sessions and the 20260828-013025 results that G2.1 used before
the `$TMPDIR/freerouting/freerouting.json` leak was closed.
`.ses` files were gitignored; this archive keeps whatever was
still on disk plus a pointer at the logs/results.json.
