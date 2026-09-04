#!/usr/bin/env bash
# Download Freerouting 2.3.0 and pin it by sha256.
# Default dest: /tmp/frbase/freerouting-2.3.0.jar (overridable).
# Authority for the hash is ci/pins.json; when infra lands bench/reference.json,
# the two must be aligned (see REPORT-ci.md).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PINS="$ROOT/ci/pins.json"
DEST=${1:-}
URL=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["jar"]["url"])' "$PINS")
WANT=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["jar"]["sha256"])' "$PINS")
PIN_PATH=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["jar"]["path"])' "$PINS")
DEST=${DEST:-$PIN_PATH}

hash_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

mkdir -p "$(dirname "$DEST")"

if [ -f "$DEST" ]; then
  GOT=$(hash_of "$DEST")
  if [ "$GOT" = "$WANT" ]; then
    echo "jar already present: $DEST"
    echo "sha256 $GOT"
    exit 0
  fi
  echo "jar at $DEST has sha256 $GOT, expected $WANT — replacing" >&2
  rm -f "$DEST"
fi

# Prefer a local copy (infra / this machine's fr-ref) over a network fetch.
for candidate in \
    "$HOME/fr-ref/frbase/freerouting-2.3.0.jar" \
    "/tmp/frbase/freerouting-2.3.0.jar"
do
  if [ -f "$candidate" ]; then
    GOT=$(hash_of "$candidate")
    if [ "$GOT" = "$WANT" ]; then
      cp -f "$candidate" "$DEST"
      echo "copied $candidate -> $DEST"
      echo "sha256 $GOT"
      exit 0
    fi
  fi
done

TMP="$DEST.partial.$$"
echo "downloading $URL"
if command -v curl >/dev/null 2>&1; then
  curl -fL --retry 3 --retry-delay 2 -o "$TMP" "$URL"
else
  wget -O "$TMP" "$URL"
fi
GOT=$(hash_of "$TMP")
if [ "$GOT" != "$WANT" ]; then
  rm -f "$TMP"
  echo "sha256 mismatch: got $GOT want $WANT" >&2
  exit 1
fi
mv "$TMP" "$DEST"
echo "fetched $DEST"
echo "sha256 $GOT"
