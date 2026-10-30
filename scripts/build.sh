#!/usr/bin/env bash
set -uo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/build.sh [linux|macos|windows|all] [options]

Build proxy-rs in release mode. The default platform is the current host.

Options:
  -p, --platform PLATFORM  linux, macos, windows, or all
  -a, --arch ARCH          x86_64 or aarch64 (default: host architecture)
  -t, --target TRIPLE      Build one explicit Rust target (overrides --arch)
      --install-target     Run `rustup target add` for each non-native target
  -h, --help               Show this help

Examples:
  scripts/build.sh
  scripts/build.sh linux --arch aarch64 --install-target
  scripts/build.sh all --arch x86_64
  scripts/build.sh windows --target x86_64-pc-windows-gnu

Native builds use Cargo's default target directory: target/release/.
Cross builds use target/<rust-target>/release/. Windows cross builds need a
compatible linker; macOS builds need the Apple SDK/toolchain. Installing a
Rust target adds its standard library only and does not install linkers/SDKs.
Builds continue after an individual failure and return non-zero if any failed.
EOF
}

fail() { printf 'Error: %s\n' "$*" >&2; exit 2; }

platform=""
platform_explicit=0
arch=""
target=""
install_target=0
while (($#)); do
  case "$1" in
    -p|--platform) (($# >= 2)) || fail "missing value for $1"; platform=$2; platform_explicit=1; shift 2 ;;
    -a|--arch) (($# >= 2)) || fail "missing value for $1"; arch=$2; shift 2 ;;
    -t|--target) (($# >= 2)) || fail "missing value for $1"; target=$2; shift 2 ;;
    --install-target) install_target=1; shift ;;
    -h|--help) usage; exit 0 ;;
    linux|macos|windows|all)
      [[ -z "$platform" ]] || fail "platform specified more than once"
      platform=$1; platform_explicit=1; shift ;;
    *) fail "unknown argument: $1 (use --help)" ;;
  esac
done

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P) || fail "cannot locate scripts directory"
repo_root=$(cd -- "$script_dir/.." && pwd -P) || fail "cannot locate repository root"
cd "$repo_root" || fail "cannot enter repository root: $repo_root"
command -v cargo >/dev/null 2>&1 || fail "cargo was not found in PATH"
command -v rustc >/dev/null 2>&1 || fail "rustc was not found in PATH"

host=$(rustc -vV | sed -n 's/^host: //p' | head -n 1)
[[ -n "$host" ]] || fail "could not determine Rust host target"
host_arch=${host%%-*}
if [[ -n "$target" && $platform_explicit == 0 ]]; then
  case "$target" in *-apple-darwin) platform=macos ;; *-windows-*) platform=windows ;; *-linux-*) platform=linux ;; *) fail "cannot infer platform from target '$target'" ;; esac
fi
[[ -n "$platform" ]] || case "$host" in
  *-apple-darwin) platform=macos ;;
  *-windows-*) platform=windows ;;
  *-linux-*) platform=linux ;;
  *) fail "unsupported host target: $host (select --platform explicitly)" ;;
esac
[[ -n "$arch" ]] || arch=$host_arch
[[ "$arch" == x86_64 || "$arch" == aarch64 ]] || fail "unsupported architecture '$arch' (choose x86_64 or aarch64)"
case "$platform" in linux|macos|windows|all) ;; *) fail "unsupported platform '$platform'" ;; esac
if [[ -n "$target" && "$platform" == all ]]; then fail "--target selects one build and cannot be combined with all"; fi
target_root=${CARGO_TARGET_DIR:-target}
if [[ "$target_root" != /* ]]; then target_root="$repo_root/$target_root"; fi

triple_for() {
  local p=$1 a=$2
  if [[ "$host_arch" == "$a" ]]; then
    case "$p:$host" in
      linux:*-unknown-linux-*) printf '%s' "$host"; return ;;
      macos:*-apple-darwin) printf '%s' "$host"; return ;;
      windows:*-pc-windows-*) printf '%s' "$host"; return ;;
    esac
  fi
  case "$p:$a" in
    linux:x86_64) printf x86_64-unknown-linux-gnu ;;
    linux:aarch64) printf aarch64-unknown-linux-gnu ;;
    macos:x86_64) printf x86_64-apple-darwin ;;
    macos:aarch64) printf aarch64-apple-darwin ;;
    windows:x86_64) printf x86_64-pc-windows-gnu ;;
    windows:aarch64) printf aarch64-pc-windows-msvc ;;
  esac
}

target_platform() {
  case "$1" in *-apple-darwin) printf macos ;; *-windows-*) printf windows ;; *-linux-*) printf linux ;; *) return 1 ;; esac
}

build_one() {
  local p=$1 t=$2 host_build=0 bin out
  local inferred
  inferred=$(target_platform "$t") || { printf 'Error: cannot infer platform from target %s\n' "$t" >&2; return 1; }
  [[ "$inferred" == "$p" ]] || { printf 'Error: target %s is for %s, not %s\n' "$t" "$inferred" "$p" >&2; return 1; }
  if [[ "$t" == "$host" ]]; then host_build=1; fi
  printf '\n==> Building %s (%s)%s\n' "$p" "$t" "$([[ $host_build == 1 ]] && printf ' [native]' || printf ' [cross]')"
  if [[ $host_build == 0 && $install_target == 1 ]]; then
    command -v rustup >/dev/null 2>&1 || { printf 'Error: --install-target requested but rustup was not found\n' >&2; return 1; }
    rustup target add "$t" || { printf 'Error: rustup could not install target %s\n' "$t" >&2; return 1; }
  fi
  local -a cmd=(cargo build --workspace --release)
  [[ $host_build == 1 ]] || cmd+=(--target "$t")
  "${cmd[@]}" || { printf 'Error: build failed for %s (%s)\n' "$p" "$t" >&2; return 1; }
  if [[ $host_build == 1 ]]; then out="$target_root/release"; else out="$target_root/$t/release"; fi
  bin=proxy-rs; [[ "$p" == windows ]] && bin=proxy-rs.exe
  if [[ ! -f "$out/$bin" ]]; then printf 'Error: build reported success but binary is missing: %s/%s\n' "$out" "$bin" >&2; return 1; fi
  printf 'Success: %s\n' "$out/$bin"
}

failures=0
if [[ -n "$target" ]]; then
  inferred=$(target_platform "$target") || fail "cannot infer platform from target '$target'"
  [[ "$inferred" == "$platform" ]] || fail "target '$target' does not match platform '$platform'"
  build_one "$platform" "$target" || failures=$((failures + 1))
else
  if [[ "$platform" == all ]]; then platforms=(linux macos windows); else platforms=("$platform"); fi
  for p in "${platforms[@]}"; do
    t=$(triple_for "$p" "$arch")
    build_one "$p" "$t" || failures=$((failures + 1))
  done
fi

if ((failures)); then printf '\nBuild finished with %d failure(s).\n' "$failures" >&2; exit 1; fi
printf '\nAll requested builds succeeded.\n'
