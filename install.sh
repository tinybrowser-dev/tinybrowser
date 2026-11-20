#!/bin/sh
# Install tinybrowser from GitHub Releases.
#
# Environment overrides:
#   TINYBROWSER_VERSION      Tag to install (e.g. "0.1.0" or "v0.1.0"). Default: latest release.
#   TINYBROWSER_INSTALL_DIR  Destination directory. Default: $PREFIX/bin on Termux, else ~/.local/bin.
set -eu

REPO="tinybrowser-dev/tinybrowser"

fail() {
  printf 'error: %s\n' "$1" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os:$arch" in
    Linux:x86_64)
      printf 'x86_64-unknown-linux-gnu'
      ;;
    *)
      fail "no prebuilt binary for ${os}/${arch} yet (supported: Linux x86_64). Build from source: https://github.com/${REPO}#building-from-source"
      ;;
  esac
}

resolve_version() {
  if [ -n "${TINYBROWSER_VERSION:-}" ]; then
    printf '%s' "${TINYBROWSER_VERSION#v}"
    return
  fi
  # Follow the /releases/latest redirect to read the tag without hitting API rate limits.
  effective="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")"
  tag="${effective##*/}"
  case "$tag" in
    v*) printf '%s' "${tag#v}" ;;
    *) fail "could not determine latest release tag from ${effective}" ;;
  esac
}

verify_sha256() {
  file="$1"
  sums="$2"
  expected="$(awk -v name="$(basename "$file")" '$2 == name { print $1; exit }' "$sums")"
  [ -n "$expected" ] || fail "checksum entry not found for $(basename "$file")"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file" | awk '{ print $1 }')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$file" | awk '{ print $1 }')"
  else
    fail "missing sha256sum or shasum to verify download"
  fi
  [ "$actual" = "$expected" ] || fail "checksum mismatch for $(basename "$file") (expected ${expected}, got ${actual})"
}

main() {
  need_cmd curl
  need_cmd tar

  target="$(detect_target)"
  version="$(resolve_version)"
  tag="v${version}"
  archive="tinybrowser-${version}-${target}.tar.gz"
  base_url="https://github.com/${REPO}/releases/download/${tag}"

  if [ -n "${TINYBROWSER_INSTALL_DIR:-}" ]; then
    install_dir="$TINYBROWSER_INSTALL_DIR"
  elif [ -n "${PREFIX:-}" ] && [ -d "${PREFIX}/bin" ]; then
    install_dir="${PREFIX}/bin"
  else
    install_dir="${XDG_BIN_HOME:-$HOME/.local/bin}"
  fi

  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT INT TERM

  printf 'Downloading tinybrowser %s (%s)...\n' "$version" "$target"
  curl -fsSL "${base_url}/${archive}" -o "${tmp}/${archive}"
  curl -fsSL "${base_url}/sha256sums.txt" -o "${tmp}/sha256sums.txt"
  verify_sha256 "${tmp}/${archive}" "${tmp}/sha256sums.txt"

  tar -xzf "${tmp}/${archive}" -C "$tmp"
  mkdir -p "$install_dir"
  install -m 755 "${tmp}/tinybrowser-${version}-${target}/tinybrowser" "${install_dir}/tinybrowser"

  printf 'Installed %s\n' "${install_dir}/tinybrowser"
  "${install_dir}/tinybrowser" --version

  case ":${PATH}:" in
    *":${install_dir}:"*) ;;
    *) printf 'Note: add %s to your PATH to run `tinybrowser` directly.\n' "$install_dir" ;;
  esac
}

main "$@"
