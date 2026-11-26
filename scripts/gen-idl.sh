#!/usr/bin/env bash
# Generate the Anchor IDLs (target/idl/<name>.json, spec 0.1.0) and TS types
# (target/types/<name>.ts) of perps-core, orderbook, vamm, prop-amm and
# mm-registry, as `anchor build` 1.2.2 would (scripts/idlgen; docs/lp-pool.md
# D49).
#
# Two packages build without target/ and keep a committed copy instead:
# the frontend (perps-core's, frontend/src/idl/) and the mm-registry SDK
# (mm-registry's, sdk/mm-registry/src/idl/). --sync refreshes those copies;
# --check fails if they differ from the fresh IDLs (CI).
#
# The builder compiles each program's lib tests with `idl-build`, and lib
# tests link the dev-dependencies (solana-program-test 4.4 is several GB in
# debug and needs Rust 1.98). Like scripts/build-sbf.sh, each program is
# therefore built from a copy without them, in target/idl-build, with the
# workspace Cargo.lock's versions when there is one. Any host Rust that builds
# anchor-lang 1.2.2 works (the SBF toolchain's 1.95 or newer).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
MODE=
for arg in "$@"; do
  case "$arg" in
    --sync) MODE=sync ;;
    --check) MODE=check ;;
    *) echo "usage: $0 [--sync | --check]" >&2; exit 2 ;;
  esac
done
PROGRAMS="perps-core orderbook vamm prop-amm mm-registry"
WORK=$ROOT/target/idl-build
mkdir -p "$WORK" "$ROOT/target/idl" "$ROOT/target/types"

cargo build --release --quiet --manifest-path "$ROOT/scripts/idlgen/Cargo.toml"
IDLGEN=$ROOT/scripts/idlgen/target/release/idlgen

# Every copy first: mm-registry, orderbook and vamm depend on ../perps-core,
# and prop-amm include!s ../../perps-core/src/{price,pyth_receiver}.rs
for prog in $PROGRAMS; do
  dst=$WORK/$prog
  rm -rf "$dst/src"
  mkdir -p "$dst"
  cp -r "$ROOT/programs/$prog/src" "$dst/src"
  python3 - "$ROOT/programs/$prog/Cargo.toml" "$dst/Cargo.toml" <<'PY'
import sys, pathlib
src, dst = sys.argv[1], sys.argv[2]
s = pathlib.Path(src).read_text()
if "[dev-dependencies]" in s:
    s = s[: s.index("[dev-dependencies]")].rstrip() + "\n"
s = s.replace("[profile.release]\noverflow-checks = true\n", "")
s += "\n[workspace]\n"
pathlib.Path(dst).write_text(s)
PY
  if [ -f "$ROOT/Cargo.lock" ]; then
    cp "$ROOT/Cargo.lock" "$dst/Cargo.lock"
  fi
done

for prog in $PROGRAMS; do
  name=${prog//-/_}
  echo "IDL: $prog"
  CARGO_TARGET_DIR=$WORK/target "$IDLGEN" "$WORK/$prog" "$ROOT/target/idl/$name.json" "$ROOT/target/types/$name.ts"
done

# <package dir>:<IDL name>
COPIES="frontend/src/idl:perps_core sdk/mm-registry/src/idl:mm_registry"
for copy in $COPIES; do
  dir=$ROOT/${copy%%:*}
  name=${copy##*:}
  case "$MODE" in
    sync)
      mkdir -p "$dir"
      cp "$ROOT/target/idl/$name.json" "$ROOT/target/types/$name.ts" "$dir/"
      echo "IDL: refreshed ${copy%%:*}/$name.{json,ts}"
      ;;
    check)
      diff -u "$dir/$name.json" "$ROOT/target/idl/$name.json"
      diff -u "$dir/$name.ts" "$ROOT/target/types/$name.ts"
      echo "IDL: ${copy%%:*}/$name.{json,ts} up to date"
      ;;
  esac
done
