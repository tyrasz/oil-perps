#!/usr/bin/env bash
# Build perps-core, prop-amm, mm-registry and orderbook to SBF shared objects
# (target/deploy/*.so).
#
#   scripts/build-sbf.sh --localnet
# instead builds the binaries of a live local run (docs/localnet.md) into
# target/deploy-localnet (never target/deploy): perps-core with the
# `localnet` feature (it then accepts session::SCHEDULE_TEST_HOURLY), the
# other three as above, plus vamm and the test-only mock-pyth oracle. Neither
# is part of the default build, and nothing in target/deploy-localnet may be
# deployed to a public cluster.
#
# Needs the Agave 4.4.1 CLI tools on PATH (cargo-build-sbf 4.5.0, which
# installs platform-tools v1.57 = Rust 1.95 under ~/.cache/solana on first
# use) and a host cargo >= 1.84 for MSRV-aware resolution. Every binary is
# sBPF v3 (`--arch v3`, checked below): once SIMD-0500 is active a cluster
# refuses to deploy or upgrade v0-v2 programs (docs/lp-pool.md D48). Each
# program is built from a standalone copy without its dev-dependencies: the
# in-process test crates (solana-program-test 4.4 etc.) need Rust 1.98, newer
# than the SBF toolchain's 1.95.
#
# Then run the end-to-end tests against the real binaries with
#   SBF_OUT_DIR=$PWD/target/deploy cargo test -p perps-core -p prop-amm -p mm-registry -p orderbook --test integration
#
# A frame over the 4 KB SBF stack does not fail cargo-build-sbf (it exits 0),
# and the program then corrupts memory at runtime, so the message fails the
# build here. platform-tools v1.57 words it "Function <f> overflows the
# maximum allowed frame space by accessing an offset ..." (v1.41's was "Error:
# ... Stack offset of ..."); both are matched. Solana 1.18's exemption for
# solana-program's frozen-abi AbiEnumVisitor is gone: nothing reports it now.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
LOCALNET=0
for arg in "$@"; do
  case "$arg" in
    --localnet) LOCALNET=1 ;;
    *) echo "usage: $0 [--localnet]" >&2; exit 2 ;;
  esac
done
PROGRAMS="perps-core prop-amm mm-registry orderbook"
if [ "$LOCALNET" = 1 ]; then
  # SBF_OUT_DIR is not honoured here: the localnet binaries must never land
  # in target/deploy, where the default build and its tests look
  OUT=$ROOT/target/deploy-localnet
  PROGRAMS="$PROGRAMS vamm mock-pyth"
else
  OUT=${SBF_OUT_DIR:-$ROOT/target/deploy}
fi
WORK=${SBF_WORK_DIR:-$ROOT/target/sbf-build}
mkdir -p "$OUT" "$WORK"

# perps-core first: prop-amm include!s ../../perps-core/src/{price,pyth_receiver}.rs,
# mock-pyth #[path]s ../../perps-core/src/pyth_receiver.rs, and in $WORK
# mm-registry's and orderbook's `path = "../perps-core"` resolves to the
# stripped copy built here.
for prog in $PROGRAMS; do
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
s = s.replace('edition = "2021"', 'edition = "2021"\nrust-version = "1.95"', 1)
s += "\n[workspace]\n\n[profile.release]\noverflow-checks = true\n"
pathlib.Path(dst).write_text(s)
PY

  (
    cd "$dst"
    # Dependency versions that support the SBF toolchain's Rust 1.95 (the
    # blake3/ahash pins and the lockfile v4 -> v3 rewrite that Solana 1.18's
    # Rust 1.75 needed are gone)
    CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback cargo generate-lockfile
    # No array here: bash < 4.4 (macOS /bin/bash 3.2) treats an empty "${a[@]}"
    # as unset under set -u
    # sBPF v3 and platform-tools v1.57 are cargo-build-sbf 4.5's defaults;
    # spelled out so a newer CLI cannot change them silently
    if [ "$LOCALNET" = 1 ] && [ "$prog" = perps-core ]; then
      cargo-build-sbf --arch v3 --tools-version v1.57 --sbf-out-dir "$OUT" --features localnet 2>&1 | tee build.log
    else
      cargo-build-sbf --arch v3 --tools-version v1.57 --sbf-out-dir "$OUT" 2>&1 | tee build.log
    fi
  )
  # Not `! grep`: set -e ignores a negated command's status
  if grep -Eq "Stack offset|overflows the maximum allowed frame space" "$dst/build.log"; then
    echo "error: $prog exceeds the SBF stack limit (see $dst/build.log)" >&2
    exit 1
  fi
done

# Every binary must be sBPF v3: the ELF header's e_flags (bytes 48..52, LE)
# carry the sBPF version
for so in "$OUT"/*.so; do
  flags=$(python3 -c "import sys; print(int.from_bytes(open(sys.argv[1], 'rb').read()[48:52], 'little'))" "$so")
  if [ "$flags" != 3 ]; then
    echo "error: $so is sBPF v$flags, not v3" >&2
    exit 1
  fi
done

ls -l "$OUT"/*.so
