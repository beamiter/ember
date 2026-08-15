#!/usr/bin/env bash
# Remove ember and its Linux desktop integration.

set -Eeuo pipefail

APP_ID="io.github.beamiter.ember"
HOME_DIR="${HOME:-}"
DESTDIR="${DESTDIR:-}"
PREFIX="${HOME_DIR}/.local"
BIN_DIR=""
DRY_RUN=0

usage() {
    cat <<'USAGE'
Usage: ./scripts/uninstall.sh [options]

Options:
  --prefix PATH          Runtime prefix (default: ~/.local)
  --bin-dir PATH         Runtime binary directory (default: PREFIX/bin)
  --dry-run              Print commands without changing files
  -h, --help             Show this help

Environment:
  DESTDIR                Optional staging root for packaging
USAGE
}

die() {
    printf 'ember uninstall: %s\n' "$*" >&2
    exit 1
}

print_command() {
    printf '  '
    printf '%q ' "$@"
    printf '\n'
}

run() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        "$@"
    fi
}

remove_file() {
    local path="$1"
    if [[ -e "${path}" || -L "${path}" ]]; then
        run rm -f -- "${path}"
    fi
}

while (($# > 0)); do
    case "$1" in
        --prefix)
            (($# >= 2)) || die "--prefix requires a path"
            PREFIX="$2"
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            shift
            ;;
        --bin-dir)
            (($# >= 2)) || die "--bin-dir requires a path"
            BIN_DIR="$2"
            shift 2
            ;;
        --bin-dir=*)
            BIN_DIR="${1#*=}"
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            (($# == 0)) || die "unexpected positional arguments: $*"
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

[[ -n "${HOME_DIR}" ]] || die "HOME is not set"
[[ "${PREFIX}" == /* ]] || die "--prefix must be an absolute path"
if [[ -z "${BIN_DIR}" ]]; then
    BIN_DIR="${PREFIX}/bin"
fi
[[ "${BIN_DIR}" == /* ]] || die "--bin-dir must be an absolute path"
if [[ -n "${DESTDIR}" ]]; then
    [[ "${DESTDIR}" == /* ]] || die "DESTDIR must be an absolute path"
    DESTDIR="${DESTDIR%/}"
fi

SHARE_DIR="${DESTDIR}${PREFIX}/share"
remove_file "${DESTDIR}${BIN_DIR}/ember"
remove_file "${SHARE_DIR}/applications/${APP_ID}.desktop"
remove_file "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
remove_file "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
remove_file "${SHARE_DIR}/icons/hicolor/128x128/apps/${APP_ID}.png"
remove_file "${SHARE_DIR}/icons/hicolor/256x256/apps/${APP_ID}.png"
# Desktop integration from before the jterm2 -> ember rename.
remove_file "${SHARE_DIR}/applications/io.github.beamiter.jterm2.desktop"
remove_file "${SHARE_DIR}/metainfo/io.github.beamiter.jterm2.metainfo.xml"
remove_file "${SHARE_DIR}/icons/hicolor/scalable/apps/io.github.beamiter.jterm2.svg"
remove_file "${SHARE_DIR}/icons/hicolor/128x128/apps/io.github.beamiter.jterm2.png"
remove_file "${SHARE_DIR}/icons/hicolor/256x256/apps/io.github.beamiter.jterm2.png"

# Without this the launcher keeps offering a dead entry and a cached icon.
if [[ -z "${DESTDIR}" ]] && ((DRY_RUN == 0)); then
    if command -v update-desktop-database >/dev/null 2>&1 \
        && [[ -d "${SHARE_DIR}/applications" ]]; then
        (umask 022 && update-desktop-database "${SHARE_DIR}/applications") \
            >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1 \
        && [[ -d "${SHARE_DIR}/icons/hicolor" ]]; then
        (umask 022 && gtk-update-icon-cache --force --ignore-theme-index --quiet \
            "${SHARE_DIR}/icons/hicolor") >/dev/null 2>&1 || true
    fi
fi

CONFIG_DIR="${XDG_CONFIG_HOME:-${HOME_DIR}/.config}/ember"
printf 'Removed ember from %s\n' "${BIN_DIR}"
printf 'Preserved configuration and history under %s\n' "${CONFIG_DIR}"
