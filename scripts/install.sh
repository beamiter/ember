#!/usr/bin/env bash
# Install ember and its Linux desktop integration from a source checkout.

set -Eeuo pipefail
umask 077

APP_ID="io.github.beamiter.ember"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
HOME_DIR="${HOME:-}"
DESTDIR="${DESTDIR:-}"
DESTDIR_ACTIVE=0
if [[ -n "${DESTDIR}" ]]; then
    DESTDIR_ACTIVE=1
fi
PREFIX="${HOME_DIR}/.local"
BIN_DIR=""
DATA_HOME=""
PREBUILT_BINARY=""
PREBUILT_FD=""
INSTALL_DESKTOP=1
DRY_RUN=0
INSTALL_TEMP=""

usage() {
    cat <<'USAGE'
Usage: ./scripts/install.sh [options]

Options:
  --prefix PATH          Runtime prefix (default: ~/.local)
  --bin-dir PATH         Runtime binary directory (default: PREFIX/bin)
  --data-dir PATH        Shared-data base (default: $XDG_DATA_HOME or PREFIX/share)
  --binary PATH          Install a prebuilt ember binary instead of building
  --no-desktop           Do not install desktop, AppStream, or icon files
  --dry-run              Print commands without changing files
  -h, --help             Show this help

Environment:
  DESTDIR                Optional staging root for packaging
  XDG_DATA_HOME          Shared-data base (default: PREFIX/share)
  CARGO_TARGET_DIR       Cargo target directory when building (default: <repo>/target)
USAGE
}

die() {
    printf 'ember install: %s\n' "$*" >&2
    exit 1
}

cleanup_install_temp() {
    if [[ -n "${INSTALL_TEMP:-}" ]]; then
        rm -f -- "${INSTALL_TEMP}"
        INSTALL_TEMP=""
    fi
}

trap cleanup_install_temp EXIT

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

# Staging and uninstall paths are assembled by string concatenation. Reject a
# lexical parent component before doing so: `/opt/../etc` is absolute, but when
# prefixed with DESTDIR it can escape the package root. Spaces, repeated slashes
# and non-ASCII names remain valid.
validate_absolute_path() {
    local label="$1" path="$2"
    [[ -n "${path}" ]] || die "${label} must not be empty"
    [[ "${path}" == /* ]] || die "${label} must be an absolute path"
    if [[ "${path}" =~ [[:cntrl:]] ]]; then
        die "${label} must not contain control characters"
    fi
    case "/${path#/}/" in
        */../*) die "${label} must not contain '..' path components" ;;
    esac
}

normalize_absolute_path() {
    local path="$1" normalized="" component
    local -a components=()
    IFS='/' read -r -a components <<<"${path}"
    for component in "${components[@]}"; do
        [[ -n "${component}" && "${component}" != . ]] || continue
        normalized="${normalized}/${component}"
    done
    printf '%s' "${normalized:-/}"
}

# Walk the normalized staging root from `/` and reject every existing symlink
# component. This is an existing-state preflight, not a concurrent-race claim.
validate_destdir_root() {
    local suffix current="" component
    local -a components=()
    ((DESTDIR_ACTIVE == 1)) || return 0
    [[ -n "${DESTDIR}" && "${DESTDIR}" != / ]] || return 0
    suffix="${DESTDIR#/}"
    IFS='/' read -r -a components <<<"${suffix}"
    for component in "${components[@]}"; do
        [[ -n "${component}" ]] || continue
        current="${current}/${component}"
        [[ ! -L "${current}" ]] \
            || die "DESTDIR path contains a symbolic-link component: ${current}"
        [[ -e "${current}" ]] || break
    done
}

require_source_file() {
    local source="$1"
    [[ ! -L "${source}" && -f "${source}" && -r "${source}" ]] \
        || die "required install source is not a readable regular file: ${source}"
}

# DESTDIR is the packaging boundary under the caller's control. Reject an
# already-present symlink in that staged path before the build starts; normal
# host installs are intentionally not subjected to this policy because system
# prefixes commonly contain legitimate compatibility symlinks.
validate_staging_target() {
    local target="$1" suffix current component
    local -a components=()
    ((DESTDIR_ACTIVE == 1)) || return 0
    [[ -n "${DESTDIR}" ]] || return 0
    validate_destdir_root
    suffix="${target#"${DESTDIR}"}"
    suffix="${suffix#/}"
    current="${DESTDIR}"
    IFS='/' read -r -a components <<<"${suffix}"
    for component in "${components[@]}"; do
        [[ -n "${component}" && "${component}" != . ]] || continue
        current="${current}/${component}"
        [[ ! -L "${current}" ]] \
            || die "staged install path contains a symbolic-link ancestor: ${current}"
        [[ -e "${current}" ]] || break
    done
}

# Public resources use the same same-directory commit discipline as the main
# executable. Explicit modes are applied to the temp before rename, so the
# restrictive process umask cannot accidentally publish owner-only assets.
install_file_atomic() {
    local mode="$1" source="$2" dest="$3" directory basename
    printf '  install -m %q %q %q && mv -fT -- %q %q\n' \
        "${mode}" "${source}" "${dest}.<temporary>" "${dest}.<temporary>" "${dest}"
    ((DRY_RUN == 0)) || return 0
    directory="${dest%/*}"
    basename="${dest##*/}"
    install -d -m 0755 "${directory}"
    INSTALL_TEMP="$(mktemp "${directory}/.${basename}.install.XXXXXX")" \
        || die "cannot create temporary file beside ${dest}"
    if ! install -m "${mode}" "${source}" "${INSTALL_TEMP}"; then
        cleanup_install_temp
        die "cannot stage ${dest}"
    fi
    if ! mv -fT -- "${INSTALL_TEMP}" "${dest}"; then
        cleanup_install_temp
        die "cannot atomically replace ${dest}"
    fi
    INSTALL_TEMP=""
}

# Build the replacement beside its destination so rename is atomic and cannot
# cross filesystems. A failed copy or rename leaves the previous executable in
# place and removes the private temporary file.
install_binary_atomic() {
    local source="$1" dest="$2"
    printf '  install -m 0755 %q %q && mv -fT -- %q %q\n' \
        "${source}" "${dest}.<temporary>" "${dest}.<temporary>" "${dest}"
    ((DRY_RUN == 0)) || return 0

    INSTALL_TEMP="$(mktemp "${dest}.install.XXXXXX")" \
        || die "cannot create temporary binary beside ${dest}"
    if ! install -m 0755 "${source}" "${INSTALL_TEMP}"; then
        cleanup_install_temp
        die "cannot stage binary for ${dest}"
    fi
    if ! mv -fT -- "${INSTALL_TEMP}" "${dest}"; then
        cleanup_install_temp
        die "cannot atomically replace ${dest}"
    fi
    INSTALL_TEMP=""
}

# Bash cannot make the initial no-symlink check and open atomic. Once the open
# succeeds and pathname/descriptor identity verification succeeds, copy via the
# inherited /proc descriptor so a later pathname replacement cannot change the
# inode being copied.
pin_prebuilt_binary() {
    local requested="$1" fd_path fd_identity path_identity
    [[ -d /proc/self/fd && -r /proc/self/fd ]] \
        || die "cannot pin prebuilt binary: /proc/self/fd is unavailable"
    [[ ! -L "${requested}" ]] \
        || die "prebuilt binary must not be a symbolic link: ${requested}"
    [[ -f "${requested}" ]] \
        || die "prebuilt binary is not a regular file: ${requested}"
    [[ -r "${requested}" ]] \
        || die "prebuilt binary is not readable: ${requested}"

    exec {PREBUILT_FD}<"${requested}" \
        || die "cannot open prebuilt binary: ${requested}"
    fd_path="/proc/self/fd/${PREBUILT_FD}"
    [[ -e "${fd_path}" ]] \
        || die "cannot pin prebuilt binary: /proc/self/fd is unavailable"
    [[ -f "${fd_path}" ]] \
        || die "opened prebuilt binary is not a regular file: ${requested}"
    [[ -s "${fd_path}" ]] \
        || die "prebuilt binary must not be empty: ${requested}"
    fd_identity="$(stat -Lc '%d:%i' -- "${fd_path}")" \
        || die "cannot identify opened prebuilt binary (GNU stat required): ${requested}"
    [[ ! -L "${requested}" && -f "${requested}" ]] \
        || die "prebuilt binary changed while being opened: ${requested}"
    path_identity="$(stat -Lc '%d:%i' -- "${requested}")" \
        || die "cannot identify prebuilt binary (GNU stat required): ${requested}"
    [[ ! -L "${requested}" && "${path_identity}" == "${fd_identity}" ]] \
        || die "prebuilt binary changed while being opened: ${requested}"

    BINARY="${fd_path}"
}

bin_dir_on_path() {
    case ":${PATH:-}:" in
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
    if [[ "${path}" =~ [[:cntrl:]] ]]; then
        die "desktop executable path must not contain control characters"
    fi
}

install_desktop_entry() {
    local source="$1" dest="$2" exec_path exec_value try_exec_value desktop_dir
    exec_path="$(desktop_exec_path)"
    validate_desktop_exec_path "${exec_path}"
    exec_value="$(desktop_exec_value "${exec_path}")"
    try_exec_value="$(desktop_try_exec_value "${exec_path}")"
    printf '  install -Dm0644 (Exec=%s) %q %q\n' "${exec_path}" "${source}" "${dest}"
    ((DRY_RUN == 0)) || return 0
    desktop_dir="${dest%/*}"
    install -d -m 0755 "${desktop_dir}"
    INSTALL_TEMP="$(mktemp "${desktop_dir}/.${APP_ID}.desktop.install.XXXXXX")" \
        || die "cannot create temporary desktop entry beside ${dest}"
    if ! EMBER_DESKTOP_EXEC_VALUE="${exec_value}" \
        EMBER_DESKTOP_TRY_EXEC_VALUE="${try_exec_value}" \
        awk '
        BEGIN { exec_count = 0; try_exec_count = 0 }
        /^Exec=ember([[:space:]]|$)/ {
            exec_count++
            eq = index($0, "=")
            print substr($0, 1, eq) ENVIRON["EMBER_DESKTOP_EXEC_VALUE"] \
                substr($0, eq + 6)
            next
        }
        /^Exec=/ { exit 42 }
        /^TryExec=ember([[:space:]]|$)/ {
            try_exec_count++
            eq = index($0, "=")
            print substr($0, 1, eq) ENVIRON["EMBER_DESKTOP_TRY_EXEC_VALUE"] \
                substr($0, eq + 6)
            next
        }
        /^TryExec=/ { exit 43 }
        { print }
        END {
            if (exec_count < 1 || try_exec_count != 1) exit 44
        }
    ' "${source}" >"${INSTALL_TEMP}" \
        || ! chmod 0644 "${INSTALL_TEMP}" \
        || ! mv -fT -- "${INSTALL_TEMP}" "${dest}"; then
        cleanup_install_temp
        die "cannot atomically install desktop entry at ${dest}"
    fi
    INSTALL_TEMP=""
}

# Freshly installed entries and icons stay invisible until the shell's caches
# are rebuilt; a stale icon cache can even shadow icons that are already there.
refresh_desktop_caches() {
    if ((DESTDIR_ACTIVE == 1)); then
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
            [[ -n "${PREFIX}" ]] || die "--prefix must not be empty"
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            [[ -n "${PREFIX}" ]] || die "--prefix must not be empty"
            shift
            ;;
        --bin-dir)
            (($# >= 2)) || die "--bin-dir requires a path"
            BIN_DIR="$2"
            [[ -n "${BIN_DIR}" ]] || die "--bin-dir must not be empty"
            shift 2
            ;;
        --bin-dir=*)
            BIN_DIR="${1#*=}"
            [[ -n "${BIN_DIR}" ]] || die "--bin-dir must not be empty"
            shift
            ;;
        --data-dir)
            (($# >= 2)) || die "--data-dir requires a path"
            DATA_HOME="$2"
            [[ -n "${DATA_HOME}" ]] || die "--data-dir must not be empty"
            shift 2
            ;;
        --data-dir=*)
            DATA_HOME="${1#*=}"
            [[ -n "${DATA_HOME}" ]] || die "--data-dir must not be empty"
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
validate_absolute_path "--prefix" "${PREFIX}"
if [[ -z "${BIN_DIR}" ]]; then
    BIN_DIR="${PREFIX}/bin"
fi
validate_absolute_path "--bin-dir" "${BIN_DIR}"
if [[ -z "${DATA_HOME}" ]]; then
    DATA_HOME="${XDG_DATA_HOME:-${PREFIX}/share}"
fi
validate_absolute_path "--data-dir/XDG_DATA_HOME" "${DATA_HOME}"
if ((DESTDIR_ACTIVE == 1)); then
    validate_absolute_path "DESTDIR" "${DESTDIR}"
    DESTDIR="$(normalize_absolute_path "${DESTDIR}")"
    validate_destdir_root
    if [[ "${DESTDIR}" == / ]]; then
        DESTDIR=""
    fi
fi

validate_staging_target "${DESTDIR}${BIN_DIR}"
validate_staging_target "${DESTDIR}${DATA_HOME}"
if ((INSTALL_DESKTOP == 1)); then
    require_source_file "${REPO_ROOT}/data/${APP_ID}.desktop"
    require_source_file "${REPO_ROOT}/data/${APP_ID}.metainfo.xml"
    require_source_file "${REPO_ROOT}/data/${APP_ID}.svg"
    require_source_file "${REPO_ROOT}/data/${APP_ID}-128.png"
    require_source_file "${REPO_ROOT}/data/${APP_ID}-256.png"
fi

# Fail before building or modifying a target if the transactional install
# primitives needed later are unavailable.
require_command install
require_command mktemp
require_command mv
require_command rm
if ((INSTALL_DESKTOP == 1)); then
    require_command awk
    require_command chmod
    validate_desktop_exec_path "$(desktop_exec_path)"
fi

if [[ -n "${PREBUILT_BINARY}" ]]; then
    BINARY="${PREBUILT_BINARY}"
    printf 'Using prebuilt ember binary: %s\n' "${BINARY}"
    if ((DRY_RUN == 0)); then
        require_command stat
        pin_prebuilt_binary "${PREBUILT_BINARY}"
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

STAGED_BIN_DIR="${DESTDIR}${BIN_DIR}"
run install -d -m 0755 "${STAGED_BIN_DIR}"
install_binary_atomic "${BINARY}" "${STAGED_BIN_DIR}/ember"
if [[ -n "${PREBUILT_FD}" ]]; then
    exec {PREBUILT_FD}<&-
fi

SHARE_DIR="${DESTDIR}${DATA_HOME}"
if ((INSTALL_DESKTOP == 1)); then
    install_desktop_entry "${REPO_ROOT}/data/${APP_ID}.desktop" \
        "${SHARE_DIR}/applications/${APP_ID}.desktop"
    # Launcher left by installs from before the jterm2 -> ember rename; left in
    # place it shows up as a second "jterm2" entry beside the new one.
    run rm -f -- "${SHARE_DIR}/applications/io.github.beamiter.jterm2.desktop"
    install_file_atomic 0644 "${REPO_ROOT}/data/${APP_ID}.metainfo.xml" \
        "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
    install_file_atomic 0644 "${REPO_ROOT}/data/${APP_ID}.svg" \
        "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
    for size in 128 256; do
        install_file_atomic 0644 "${REPO_ROOT}/data/${APP_ID}-${size}.png" \
            "${SHARE_DIR}/icons/hicolor/${size}x${size}/apps/${APP_ID}.png"
    done
    refresh_desktop_caches
fi

printf 'Installed ember to %s\n' "${BIN_DIR}/ember"
if ((INSTALL_DESKTOP == 1)); then
    printf 'Installed desktop integration under %s\n' "${DATA_HOME}"
    printf 'Launcher entry: %s (Exec=%s)\n' \
        "${SHARE_DIR}/applications/${APP_ID}.desktop" "$(desktop_exec_path)"
fi
if ((DESTDIR_ACTIVE == 1)); then
    printf 'Staged file: %s\n' "${STAGED_BIN_DIR}/ember"
fi
if ((DESTDIR_ACTIVE == 0)); then
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
