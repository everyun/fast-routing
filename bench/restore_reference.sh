#!/bin/bash
# Restore the pinned Java reference (jar, release source, 46-board fixture suite).
#
# Real files live under ~/fr-ref (override with FR_REF). /tmp only holds
# directory symlinks, never the jar itself — `ln -s` of the jar onto itself
# is how a previous restore lost the 60 MB file.
#
# Idempotent: skip a download/clone whose pin already matches.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PIN="$ROOT/bench/reference.json"
REF=${FR_REF:-$HOME/fr-ref}
JAVA=/opt/homebrew/opt/openjdk/bin/java

filename=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['filename'])" "$PIN")
want_sha=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['sha256'])" "$PIN")
want_rev=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['build_revision'])" "$PIN")
want_url=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['download_url'])" "$PIN")

sha_of() {
  shasum -a 256 "$1" | awk '{print $1}'
}

echo "==> jar → $REF/frbase/$filename"
mkdir -p "$REF/frbase"
jar="$REF/frbase/$filename"
if [ -f "$jar" ] && [ ! -L "$jar" ] && [ "$(sha_of "$jar")" = "$want_sha" ]; then
  echo "    already pinned ($want_sha)"
else
  rm -f "$jar"
  if command -v gh >/dev/null 2>&1; then
    gh release download v2.3.0 -R freerouting/freerouting -p "$filename" -D "$REF/frbase" --clobber
  else
    curl -L --fail -o "$jar" "$want_url"
  fi
  got=$(sha_of "$jar")
  if [ "$got" != "$want_sha" ]; then
    echo "error: jar sha256 $got != pinned $want_sha" >&2
    exit 1
  fi
  echo "    downloaded $got"
fi
# Directory symlink only. Never `ln` the jar file.
ln -sfn "$REF/frbase" /tmp/frbase
rev=$(unzip -p "/tmp/frbase/$filename" META-INF/MANIFEST.MF | awk -F': ' '/^Build-Revision:/{print $2}' | tr -d '\r')
if [ "$rev" != "$want_rev" ]; then
  echo "error: Build-Revision $rev != pinned $want_rev" >&2
  exit 1
fi
echo "    /tmp/frbase/$filename  Build-Revision $rev"

echo "==> source → $REF/fr-java-release @$want_rev"
src="$REF/fr-java-release"
if [ -d "$src/.git" ] && [ "$(git -C "$src" rev-parse HEAD 2>/dev/null)" = "$want_rev" ]; then
  echo "    already at $want_rev"
else
  if [ ! -d "$src/.git" ]; then
    rm -rf "$src"
    git clone --filter=blob:none https://github.com/freerouting/freerouting "$src"
  fi
  git -C "$src" fetch --depth 1 origin "$want_rev" || git -C "$src" fetch origin "$want_rev"
  git -C "$src" checkout --detach "$want_rev"
fi
ln -sfn "$src" /tmp/fr-java-release
algo=/tmp/fr-java-release/src/main/java/app/freerouting/autoroute/MazeSearchAlgo.java
if [ ! -f "$algo" ]; then
  echo "error: MazeSearchAlgo.java not at $algo" >&2
  exit 1
fi
echo "    $algo"

echo "==> fixtures → $REF/freerouting-upstream/fixtures (46 boards)"
fixroot="$REF/freerouting-upstream"
mkdir -p "$fixroot/fixtures"
# Prefer the 46 names pinned by the checked-in manifest, copied from the
# already-cloned 2.3.0 tree (md5-identical to the last fetch). Fall back to
# bench/fetch_fixtures.sh when the source clone is missing.
manifest="$ROOT/bench/fixtures-manifest.json"
python3 - "$manifest" "$src/fixtures" "$fixroot/fixtures" <<'PY'
import json, os, shutil, sys
man, src, dst = sys.argv[1], sys.argv[2], sys.argv[3]
names = [e["name"] for e in json.load(open(man))["fixtures"]]
os.makedirs(dst, exist_ok=True)
missing = []
for name in names:
    s, d = os.path.join(src, name), os.path.join(dst, name)
    if os.path.isfile(s):
        if (not os.path.isfile(d)) or os.path.getsize(s) != os.path.getsize(d):
            shutil.copy2(s, d)
    elif not os.path.isfile(d):
        missing.append(name)
if missing:
    sys.exit("missing fixtures: " + ", ".join(missing[:8]))
print(f"    {len(names)} dsn in {dst}")
PY
ln -sfn "$fixroot" /tmp/freerouting-upstream
n=$(ls /tmp/freerouting-upstream/fixtures/*.dsn 2>/dev/null | wc -l | tr -d ' ')
echo "    /tmp/freerouting-upstream/fixtures  $n dsn"
if [ "$n" -lt 46 ]; then
  echo "error: expected 46 top-level dsn, found $n" >&2
  exit 1
fi

echo "ok: jar + source + fixtures restored"
echo "    java: $JAVA"
echo "    jar:  /tmp/frbase/$filename"
echo "    src:  /tmp/fr-java-release/src"
echo "    dsn:  /tmp/freerouting-upstream/fixtures ($n)"
