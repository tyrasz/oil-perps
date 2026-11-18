#!/usr/bin/env bash
# Build perps-core and prop-amm to SBF shared objects (target/deploy/*.so).
#
# Needs the Solana 1.18 CLI tools on PATH (cargo-build-sbf, platform-tools
# v1.41 = Rust 1.75) and a recent host cargo (>= 1.84) for MSRV-aware
# resolution. Each program is built from a standalone copy without its
# dev-dependencies: the in-process test crates (solana-program-test etc.) pull
# in dependencies that need a newer Rust/Cargo than the SBF toolchain has.
#
# Then run the end-to-end tests against the real binaries with
#   SBF_OUT_DIR=$PWD/target/deploy cargo test -p perps-core -p prop-amm --test integration
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${SBF_OUT_DIR:-$ROOT/target/deploy}
WORK=${SBF_WORK_DIR:-$ROOT/target/sbf-build}
mkdir -p "$OUT" "$WORK"

# perps-core first: prop-amm include!s ../../perps-core/src/price.rs
for prog in perps-core prop-amm; do
  dst="$WORK/$prog"
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
s = s.replace('edition = "2021"', 'edition = "2021"\nrust-version = "1.75"', 1)
s += "\n[workspace]\n\n[profile.release]\noverflow-checks = true\n"
pathlib.Path(dst).write_text(s)
PY

  (
    cd "$dst"
    # Prefer dependency versions that support Rust 1.75 ...
    CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback cargo generate-lockfile
    # ... and pin the ones that don't declare it (blake3 >= 1.6 needs edition 2024 crates)
    cargo update -p blake3 --precise 1.5.0
    # platform-tools' cargo 1.75 reads lockfile v3, not v4
    python3 -c "import pathlib; p = pathlib.Path('Cargo.lock'); p.write_text(p.read_text().replace('\nversion = 4\n', '\nversion = 3\n', 1))"
    cargo-build-sbf --sbf-out-dir "$OUT"
  )
done

ls -l "$OUT"/*.so
