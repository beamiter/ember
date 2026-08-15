#!/usr/bin/env bash
# Install ember and its Linux desktop integration from a source checkout.

set -Eeuo pipefail
umask 077

APP_ID="io.github.beamiter.ember"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
HOME_DIR="${HOME:-}"
DESTDIR="${DESTDIR:-}"
PREFIX="${HOME_DIR}/.local"
BIN_DIR=""
PREBUILT_BINARY=""
INSTALL_DESKTOP=1
DRY_RUN=0

usage() {
    cat <<'USAGE'
Usage: ./scripts/install.sh [options]

Options:
  --prefix PATH          Runtime prefix (default: ~/.local)
  --bin-dir PATH         Runtime binary directory (default: PREFIX/bin)
  --binary PATH          Install a prebuilt ember binary instead of building
  --no-desktop           Do not install desktop, AppStream, or icon files
  --dry-run              Print commands without changing files
  -h, --help             Show this help

Environment:
  DESTDIR                Optional staging root for packaging
  CARGO_TARGET_DIR       Cargo target directory when building (default: <repo>/target)
USAGE
}

die() {
    printf 'ember install: %s\n' "$*" >&2
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

run_optional() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        "$@" || printf 'ember install: warning: %s failed (non-fatal)\n' "$1" >&2
    fi
}

# Like run_optional, but relaxes this script's restrictive umask: the desktop
# and icon caches are generated files that every user of a shared prefix has to
# be able to read.
run_optional_public() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        (umask 022 && "$@") \
            || printf 'ember install: warning: %s failed (non-fatal)\n' "$1" >&2
    fi
}

run_in_repo() {
    printf '  (cd %q && ' "${REPO_ROOT}"
    printf '%q ' "$@"
    printf ')\n'
    if ((DRY_RUN == 0)); then
        (cd -- "${REPO_ROOT}" && "$@")
    fi
}

require_command() {
    if command -v "$1" >/dev/null 2>&1; then
        return
    fi
    ((DRY_RUN == 1)) || die "required command not found: $1"
}

bin_dir_on_path() {
    case ":${PATH}:" in
        *":${BIN_DIR}:"*) return 0 ;;
        *) return 1 ;;
    esac
}

# A desktop session fixes its PATH at login, so an entry that only says
# `Exec=ember` fails TryExec and is hidden from the launcher whenever the
# binary lives in a per-user bin dir that PATH does not list. Point the entry at
# the real path unless the target is a system bin dir that is always on PATH.
desktop_exec_path() {
    case "${BIN_DIR}" in
        /usr/bin | /usr/local/bin | /bin) printf 'ember' ;;
        *) printf '%s/ember' "${BIN_DIR}" ;;
    esac
}

# Exec is a command line, not a plain path. Always quote an absolute program
# path and apply both layers of escaping required by the Desktop Entry spec:
# generic string decoding happens before Exec quoting/field-code expansion.
desktop_exec_value() {
    local remaining="$1" escaped="" character
    if [[ "${remaining}" == ember ]]; then
        printf 'ember'
        return
    fi
    while [[ -n "${remaining}" ]]; do
        character="${remaining:0:1}"
        remaining="${remaining:1}"
        case "${character}" in
            \\) escaped="${escaped}\\\\\\\\" ;;
            '"') escaped+='\"' ;;
            '`') escaped+='\`' ;;
            '$') escaped+='\\$' ;;
            *) escaped+="${character}" ;;
        esac
    done
    printf '"%s"' "${escaped}"
}

# TryExec is a plain desktop-entry string rather than a command line. Only the
# generic string layer applies, so a literal backslash needs one doubling.
desktop_try_exec_value() {
    local remaining="$1" escaped="" character
    while [[ -n "${remaining}" ]]; do
        character="${remaining:0:1}"
        remaining="${remaining:1}"
        case "${character}" in
            \\) escaped="${escaped}\\\\" ;;
            *) escaped+="${character}" ;;
        esac
    done
    printf '%s' "${escaped}"
}

validate_desktop_exec_path() {
    local path="$1"
    [[ "${path}" != *'='* ]] \
        || die "desktop executable path must not contain '=': ${path}"
    # A literal percent is written as `%%`, but the specification leaves field
    # codes inside a quoted argument undefined. Absolute paths are quoted so
    # spaces and other reserved characters work; accepting `%` here therefore
    # creates entries that validate yet fail to launch in common GLib desktops.
    [[ "${path}" != *'%'* ]] \
        || die "desktop executable path must not contain '%': ${path}"
    if LC_ALL=C grep -q '[[:cntrl:]]' <<<"${path}"; then
        die "desktop executable path must not contain control characters"
    fi
}

install_desktop_entry() {
    local source="$1" dest="$2" exec_path exec_value try_exec_value
    exec_path="$(desktop_exec_path)"
    validate_desktop_exec_path "${exec_path}"
    exec_value="$(desktop_exec_value "${exec_path}")"
    try_exec_value="$(desktop_try_exec_value "${exec_path}")"
    printf '  install -Dm0644 (Exec=%s) %q %q\n' "${exec_path}" "${source}" "${dest}"
    ((DRY_RUN == 0)) || return 0
    install -d -m 0755 "$(dirname -- "${dest}")"
    EMBER_DESKTOP_EXEC_VALUE="${exec_value}" \
        EMBER_DESKTOP_TRY_EXEC_VALUE="${try_exec_value}" \
        awk '
        /^Exec=ember([[:space:]]|$)/ {
            eq = index($0, "=")
            print substr($0, 1, eq) ENVIRON["EMBER_DESKTOP_EXEC_VALUE"] \
                substr($0, eq + 6)
            next
        }
        /^TryExec=ember([[:space:]]|$)/ {
            eq = index($0, "=")
            print substr($0, 1, eq) ENVIRON["EMBER_DESKTOP_TRY_EXEC_VALUE"] \
                substr($0, eq + 6)
            next
        }
        { print }
    ' "${source}" >"${dest}.new"
    chmod 0644 "${dest}.new"
    mv -f -- "${dest}.new" "${dest}"
}

# Freshly installed entries and icons stay invisible until the shell's caches
# are rebuilt; a stale icon cache can even shadow icons that are already there.
refresh_desktop_caches() {
    if [[ -n "${DESTDIR}" ]]; then
        printf 'Staged install (DESTDIR set); skipping desktop cache refresh.\n'
        return 0
    fi
    if command -v desktop-file-validate >/dev/null 2>&1; then
        run_optional desktop-file-validate "${SHARE_DIR}/applications/${APP_ID}.desktop"
    fi
    if command -v update-desktop-database >/dev/null 2>&1; then
        run_optional_public update-desktop-database "${SHARE_DIR}/applications"
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        run_optional_public gtk-update-icon-cache --force --ignore-theme-index --quiet \
            "${SHARE_DIR}/icons/hicolor"
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
        --binary)
            (($# >= 2)) || die "--binary requires a path"
            PREBUILT_BINARY="$2"
            [[ -n "${PREBUILT_BINARY}" ]] || die "--binary must not be empty"
            shift 2
            ;;
        --binary=*)
            PREBUILT_BINARY="${1#*=}"
            [[ -n "${PREBUILT_BINARY}" ]] || die "--binary must not be empty"
            shift
            ;;
        --no-desktop)
            INSTALL_DESKTOP=0
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
[[ -n "${PREFIX}" ]] || die "prefix must not be empty"
[[ "${PREFIX}" == /* ]] || die "--prefix must be an absolute path"
if [[ -z "${BIN_DIR}" ]]; then
    BIN_DIR="${PREFIX}/bin"
fi
[[ "${BIN_DIR}" == /* ]] || die "--bin-dir must be an absolute path"
if [[ -n "${DESTDIR}" ]]; then
    [[ "${DESTDIR}" == /* ]] || die "DESTDIR must be an absolute path"
    DESTDIR="${DESTDIR%/}"
fi

if [[ -n "${PREBUILT_BINARY}" ]]; then
    BINARY="${PREBUILT_BINARY}"
    printf 'Using prebuilt ember binary: %s\n' "${BINARY}"
    if ((DRY_RUN == 0)) && [[ ! -f "${BINARY}" ]]; then
        die "prebuilt binary is not a regular file: ${BINARY}"
    fi
    if ((DRY_RUN == 0)) && [[ ! -r "${BINARY}" ]]; then
        die "prebuilt binary is not readable: ${BINARY}"
    fi
else
    TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
    if [[ "${TARGET_DIR}" != /* ]]; then
        TARGET_DIR="${REPO_ROOT}/${TARGET_DIR}"
    fi
    export CARGO_TARGET_DIR="${TARGET_DIR}"

    printf 'Building ember...\n'
    require_command cargo
    run_in_repo cargo build --release --locked

    BINARY="${TARGET_DIR}/release/ember"
    if ((DRY_RUN == 0)) && [[ ! -x "${BINARY}" ]]; then
        die "release binary was not produced at ${BINARY}"
    fi
fi

require_command install
STAGED_BIN_DIR="${DESTDIR}${BIN_DIR}"
run install -d -m 0755 "${STAGED_BIN_DIR}"
run install -m 0755 "${BINARY}" "${STAGED_BIN_DIR}/ember"

SHARE_DIR="${DESTDIR}${PREFIX}/share"
if ((INSTALL_DESKTOP == 1)); then
    install_desktop_entry "${REPO_ROOT}/data/${APP_ID}.desktop" \
        "${SHARE_DIR}/applications/${APP_ID}.desktop"
    # Launcher left by installs from before the jterm2 -> ember rename; left in
    # place it shows up as a second "jterm2" entry beside the new one.
    run rm -f -- "${SHARE_DIR}/applications/io.github.beamiter.jterm2.desktop"
    run install -Dm0644 "${REPO_ROOT}/data/${APP_ID}.metainfo.xml" \
        "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
    run install -Dm0644 "${REPO_ROOT}/data/${APP_ID}.svg" \
        "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
    for size in 128 256; do
        run install -Dm0644 "${REPO_ROOT}/data/${APP_ID}-${size}.png" \
            "${SHARE_DIR}/icons/hicolor/${size}x${size}/apps/${APP_ID}.png"
    done
    refresh_desktop_caches
fi

printf 'Installed ember to %s\n' "${BIN_DIR}/ember"
if ((INSTALL_DESKTOP == 1)); then
    printf 'Installed desktop integration under %s/share\n' "${PREFIX}"
    printf 'Launcher entry: %s (Exec=%s)\n' \
        "${SHARE_DIR}/applications/${APP_ID}.desktop" "$(desktop_exec_path)"
fi
if [[ -n "${DESTDIR}" ]]; then
    printf 'Staged file: %s\n' "${STAGED_BIN_DIR}/ember"
fi
if [[ -z "${DESTDIR}" ]]; then
    if ! bin_dir_on_path; then
        printf '\nNote: %s is not in PATH; the launcher entry uses the absolute path,\n' \
            "${BIN_DIR}"
        printf 'but shells will not find ember until you add it, for example:\n'
        printf "  echo 'export PATH=\"%s:\$PATH\"' >>~/.profile\n" "${BIN_DIR}"
    fi
    SHADOWING_BIN="$(command -v ember 2>/dev/null || true)"
    if [[ -n "${SHADOWING_BIN}" && "${SHADOWING_BIN}" != "${BIN_DIR}/ember" ]]; then
        # The backticks are literal command-name markup in user-facing prose.
        # shellcheck disable=SC2016
        printf '\nNote: typing `ember` still runs %s, an older copy earlier in PATH.\n' \
            "${SHADOWING_BIN}"
        printf 'Remove it, or put %s ahead of it in PATH.\n' "${BIN_DIR}"
        printf 'The launcher entry is unaffected: it runs %s directly.\n' \
            "${BIN_DIR}/ember"
    fi
fi
