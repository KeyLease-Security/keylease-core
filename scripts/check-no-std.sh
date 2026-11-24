#!/usr/bin/env bash
#
# check-no-std.sh — prove the deployable contract Wasm is built without std.
#
# Every contract crate is #![no_std] and targets wasm32v1-none, but nothing
# else stopped a dependency, a feature flag or a target change from pulling
# the standard library into the artifact we deploy. This script fails if:
#
#   1. a crate under contracts/ is missing #![no_std] or the unsafe_code lint
#      attributes at the top of its lib.rs;
#   2. the toolchain ships a std for wasm32v1-none (then a std dependency
#      would build instead of failing, and the remaining checks would be the
#      only guard);
#   3. the release build for wasm32v1-none fails (this is where a crate that
#      needs std is caught: the target has only core and alloc);
#   4. a built .wasm imports anything other than Soroban host functions
#      (std and libc on wasm import from modules such as
#      wasi_snapshot_preview1 or env, Soroban host modules are one letter);
#   5. a built .wasm contains a std source path (library/std/), which is what
#      std's panic and formatting machinery leaves in the data section.
#
# Usage
#   ./scripts/check-no-std.sh              # build, then check
#   ./scripts/check-no-std.sh --no-build   # check artifacts already built
#
# Requires: cargo, rustc, python3. Runs in CI in the "Build (wasm32v1-none)"
# job. See CONTRIBUTING.md#unsafe-code-and-std.

set -euo pipefail

TARGET="wasm32v1-none"
BUILD=1

case "${1:-}" in
  "") ;;
  --no-build) BUILD=0 ;;
  -h|--help) sed -n '2,27p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
esac

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

FAILED=0
fail() { printf 'FAIL: %s\n' "$*" >&2; FAILED=1; }
ok()   { printf 'ok:   %s\n' "$*"; }

command -v python3 >/dev/null 2>&1 || { echo "error: python3 is required" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 1. Crate attributes
# ---------------------------------------------------------------------------

shopt -s nullglob
crates=(contracts/*/src/lib.rs)
[ "${#crates[@]}" -gt 0 ] || { echo "error: no contracts/*/src/lib.rs found" >&2; exit 1; }

for lib in "${crates[@]}"; do
  grep -qx '#!\[no_std\]' "$lib" \
    || fail "$lib: missing #![no_std]"
  grep -qx '#!\[cfg_attr(target_family = "wasm", forbid(unsafe_code))\]' "$lib" \
    || fail "$lib: missing #![cfg_attr(target_family = \"wasm\", forbid(unsafe_code))]"
  grep -qx '#!\[cfg_attr(not(target_family = "wasm"), deny(unsafe_code))\]' "$lib" \
    || fail "$lib: missing #![cfg_attr(not(target_family = \"wasm\"), deny(unsafe_code))]"
done
[ "$FAILED" = 0 ] && ok "every contract crate is #![no_std] and forbids unsafe code"

# ---------------------------------------------------------------------------
# 2. The target has no std to link
# ---------------------------------------------------------------------------

TARGET_LIB="$(rustc --print sysroot)/lib/rustlib/$TARGET/lib"
if [ ! -d "$TARGET_LIB" ]; then
  fail "$TARGET is not installed for this toolchain (rustup target add $TARGET)"
else
  std_libs=("$TARGET_LIB"/libstd-*.rlib)
  if [ "${#std_libs[@]}" -gt 0 ]; then
    fail "the $TARGET sysroot ships std (${std_libs[0]}); a std dependency would link silently"
  else
    ok "$TARGET sysroot has no std (only core/alloc)"
  fi
fi

# ---------------------------------------------------------------------------
# 3. Release build
# ---------------------------------------------------------------------------

if [ "$BUILD" = 1 ]; then
  cargo build --target "$TARGET" --release --all \
    || { fail "cargo build --target $TARGET --release --all failed (a crate may require std)"; exit 1; }
fi

wasm=(target/"$TARGET"/release/*.wasm)
[ "${#wasm[@]}" -gt 0 ] \
  || { fail "no Wasm artifacts in target/$TARGET/release (build first, or drop --no-build)"; exit 1; }

# ---------------------------------------------------------------------------
# 4 + 5. Inspect each artifact
# ---------------------------------------------------------------------------

python3 -I - "${wasm[@]}" <<'PY' || FAILED=1
import re
import sys

STD_PATH = re.compile(rb"library/std/|/std/src/")
# Soroban host functions are imported from one-character modules
# ("a", "b", "i", "l", "m", "v", "x", ...). std/libc on wasm import from
# wasi_snapshot_preview1, env, __wbindgen_placeholder__ and the like.
HOST_MODULE = re.compile(r"^[0-9a-z]$")


def leb128(buf, pos):
    result = shift = 0
    while True:
        byte = buf[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        shift += 7
        if byte < 0x80:
            return result, pos


def name(buf, pos):
    length, pos = leb128(buf, pos)
    return buf[pos:pos + length].decode("utf-8", "replace"), pos + length


def import_modules(buf):
    if buf[:4] != b"\0asm":
        raise ValueError("not a Wasm module")
    modules = set()
    pos = 8
    while pos < len(buf):
        section, pos = buf[pos], pos + 1
        size, pos = leb128(buf, pos)
        end = pos + size
        if section == 2:  # import section
            count, p = leb128(buf, pos)
            for _ in range(count):
                module, p = name(buf, p)
                _field, p = name(buf, p)
                kind, p = buf[p], p + 1
                if kind == 0:      # func: type index
                    _, p = leb128(buf, p)
                elif kind == 1:    # table: reftype + limits
                    p += 1
                    flags, p = leb128(buf, p)
                    _, p = leb128(buf, p)
                    if flags & 1:
                        _, p = leb128(buf, p)
                elif kind == 2:    # memory: limits
                    flags, p = leb128(buf, p)
                    _, p = leb128(buf, p)
                    if flags & 1:
                        _, p = leb128(buf, p)
                elif kind == 3:    # global: valtype + mutability
                    p += 2
                else:
                    raise ValueError(f"unknown import kind {kind}")
                modules.add(module)
        pos = end
    return modules


failed = False
for path in sys.argv[1:]:
    data = open(path, "rb").read()
    try:
        modules = import_modules(data)
    except (ValueError, IndexError) as err:
        print(f"FAIL: {path}: could not parse imports: {err}", file=sys.stderr)
        failed = True
        continue
    foreign = sorted(m for m in modules if not HOST_MODULE.match(m))
    if foreign:
        print(f"FAIL: {path}: imports from non-Soroban modules {foreign} (std or libc linked?)",
              file=sys.stderr)
        failed = True
    hit = STD_PATH.search(data)
    if hit:
        print(f"FAIL: {path}: contains std source path {hit.group().decode()!r} (std linked?)",
              file=sys.stderr)
        failed = True
    if not foreign and not hit:
        print(f"ok:   {path}: imports only Soroban host modules {sorted(modules)}, no std paths")

sys.exit(1 if failed else 0)
PY

if [ "$FAILED" != 0 ]; then
  echo "no_std check failed. See CONTRIBUTING.md#unsafe-code-and-std." >&2
  exit 1
fi
echo "no_std check passed."
