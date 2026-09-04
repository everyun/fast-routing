#!/usr/bin/env bash
# Compile DumpDsn.java against the reference jar.
#
# Jar resolution order: $1 > $FR_JAR > bench/reference.json ("jar" key, when a
# task provides one) > the documented default /tmp/frbase/freerouting-2.3.0.jar.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
JAR="${1:-${FR_JAR:-}}"
if [[ -z "$JAR" && -f "$HERE/../../reference.json" ]]; then
  JAR="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["jar"])' "$HERE/../../reference.json")"
fi
JAR="${JAR:-/tmp/frbase/freerouting-2.3.0.jar}"
JAVAC="${JAVAC:-/opt/homebrew/opt/openjdk/bin/javac}"
"$JAVAC" -cp "$JAR" -d "$HERE" "$HERE/DumpDsn.java"
echo "compiled DumpDsn against $JAR -> $HERE/DumpDsn.class"
