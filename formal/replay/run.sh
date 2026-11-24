#!/usr/bin/env bash
# Model-check the replay-driver model with TLC and check every expected outcome:
#   every cfg/<name>.cfg must end as expected.txt says: pass, or violated
#   (mutants, vacuity probes, and the two findings against the code at dc993fc).
# Exit 0 only if every run ends as expected. Needs Java 11+ on PATH (CI: temurin 17).
set -euo pipefail

TLA_VERSION=1.7.4
TLA_SHA256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88

here="$(cd "$(dirname "$0")" && pwd)"
jar="${TLA2TOOLS_JAR:-${XDG_CACHE_HOME:-$HOME/.cache}/tla2tools-$TLA_VERSION.jar}"
if [ ! -f "$jar" ]; then
  mkdir -p "$(dirname "$jar")"
  curl -fsSL -o "$jar.tmp" \
    "https://github.com/tlaplus/tlaplus/releases/download/v$TLA_VERSION/tla2tools.jar"
  mv "$jar.tmp" "$jar"
fi
echo "$TLA_SHA256  $jar" | sha256sum -c --quiet -

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp "$here/ReplayDriver.tla" "$work/"

fail=0
run() { # cfg expect(pass|violated)
  local cfg="$1" expect="$2" name out
  name="$(basename "$cfg" .cfg)"
  out="$work/$name.out"
  cp "$here/$cfg" "$work/$name.cfg"
  ( cd "$work" && java -XX:+UseParallelGC -cp "$jar" tlc2.TLC -workers auto \
      -metadir "$work/meta-$name" -config "$name.cfg" ReplayDriver.tla ) \
      >"$out" 2>&1 || true
  local got
  if grep -q "Model checking completed. No error has been found." "$out"; then
    got=pass
  elif grep -qE "^Error: (Invariant .* is violated|Temporal properties were violated|Action property .* is violated)" "$out"; then
    got=violated
  else
    got=error
  fi
  local states
  states="$(grep -Eo '[0-9,]+ distinct states found' "$out" | tail -1 || true)"
  if [ "$got" = "$expect" ]; then
    printf 'OK    %-44s %-8s %s\n' "$cfg" "$got" "$states"
  else
    printf 'FAIL  %-44s expected %s, got %s\n' "$cfg" "$expect" "$got"
    sed -n '1,80p' "$out"
    fail=1
  fi
  if [ -n "${TLC_LOG_DIR:-}" ]; then mkdir -p "$TLC_LOG_DIR"; cp "$out" "$TLC_LOG_DIR/"; fi
}

# expected.txt: one "<cfg name> <pass|violated>" per line.
while read -r name expect; do
  [ -n "$name" ] && run "cfg/$name.cfg" "$expect"
done < "$here/expected.txt"
exit "$fail"
