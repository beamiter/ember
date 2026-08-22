#!/usr/bin/env bash
# Exercise the install/uninstall path contract without building or touching a
# real home directory. In addition to dry-run path coverage, this performs one
# real DESTDIR round trip from a prebuilt binary fixture.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="${SCRIPT_DIR}/install.sh"
UNINSTALLER="${SCRIPT_DIR}/uninstall.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ember-install-paths.XXXXXX")"
TEST_HOME="${TEST_ROOT}/home"
TEST_PATH="/usr/bin:/bin"

trap 'rm -rf -- "${TEST_ROOT}"' EXIT
mkdir -p "${TEST_HOME}"

install_dry_run() {
    local destdir="$1"
    shift
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${destdir}" \
        CARGO_TARGET_DIR= "${INSTALLER}" --dry-run "$@"
}

uninstall_dry_run() {
    local destdir="$1"
    shift
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${destdir}" \
        "${UNINSTALLER}" --dry-run "$@"
}

assert_contains() {
    local label="$1" output="$2" expected="$3"
    if [[ "${output}" != *"${expected}"* ]]; then
        printf 'FAIL: %s did not contain %q\n%s\n' "${label}" "${expected}" "${output}" >&2
        exit 1
    fi
}

assert_same() {
    local label="$1" actual="$2" expected="$3"
    if [[ "${actual}" != "${expected}" ]]; then
        printf 'FAIL: %s differed between identical dry runs\n' "${label}" >&2
        exit 1
    fi
}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

assert_regular_file() {
    local label="$1" path="$2"
    [[ -f "${path}" ]] || fail "${label} is not a regular file: ${path}"
}

assert_mode() {
    local label="$1" path="$2" expected="$3" actual
    actual="$(stat -c '%a' -- "${path}")"
    [[ "${actual}" == "${expected}" ]] \
        || fail "${label} mode was ${actual}, expected ${expected}: ${path}"
}

assert_absent() {
    local label="$1" path="$2"
    [[ ! -e "${path}" && ! -L "${path}" ]] \
        || fail "${label} was not removed: ${path}"
}

assert_install_uninstall_pair() {
    local label="$1" expected_binary="$2"
    shift 2
    local install_output uninstall_output expected_dir
    install_output="$(install_dry_run "" "$@")"
    mkdir -p "$(dirname -- "${expected_binary}")"
    touch "${expected_binary}"
    uninstall_output="$(uninstall_dry_run "" "$@")"
    expected_dir="${expected_binary%/ember}"

    assert_contains "${label} install" "${install_output}" \
        "Installed ember to ${expected_binary}"
    assert_contains "${label} uninstall target" "${uninstall_output}" \
        "${expected_binary}"
    assert_contains "${label} uninstall" "${uninstall_output}" \
        "Removed ember from ${expected_dir}"
}

default_install="$(install_dry_run "")"
assert_install_uninstall_pair \
    "default" "${TEST_HOME}/.local/bin/ember"
assert_same "default reinstall plan" "$(install_dry_run "")" "${default_install}"

custom_prefix="${TEST_ROOT}/prefix"
assert_install_uninstall_pair \
    "explicit prefix" "${custom_prefix}/bin/ember" \
    --prefix "${custom_prefix}"

custom_bin="${TEST_ROOT}/custom-bin"
assert_install_uninstall_pair \
    "explicit bin dir" "${custom_bin}/ember" \
    --bin-dir="${custom_bin}"

combined_prefix="${TEST_ROOT}/combined-prefix"
combined_bin="${TEST_ROOT}/combined-bin"
assert_install_uninstall_pair \
    "combined overrides" "${combined_bin}/ember" \
    --prefix="${combined_prefix}" --bin-dir "${combined_bin}"

# DESTDIR changes only where files are staged. The launcher must retain the
# runtime path, and uninstall must prepend the same staging root.
stage_root="${TEST_ROOT}/stage"
runtime_prefix="/opt/ember-contract"
runtime_bin="${runtime_prefix}/bin"
mkdir -p "${stage_root}${runtime_bin}"
touch "${stage_root}${runtime_bin}/ember"

stage_install="$(install_dry_run "${stage_root}/" --prefix "${runtime_prefix}")"
stage_uninstall="$(uninstall_dry_run "${stage_root}/" --prefix "${runtime_prefix}")"
assert_contains "DESTDIR staged install" "${stage_install}" \
    "Staged file: ${stage_root}${runtime_bin}/ember"
assert_contains "DESTDIR launcher" "${stage_install}" \
    "Exec=${runtime_bin}/ember"
assert_contains "DESTDIR staged uninstall" "${stage_uninstall}" \
    "${stage_root}${runtime_bin}/ember"
assert_contains "DESTDIR runtime summary" "${stage_uninstall}" \
    "Removed ember from ${runtime_bin}"

stage_custom_bin="/opt/ember-contract-libexec"
mkdir -p "${stage_root}${stage_custom_bin}"
touch "${stage_root}${stage_custom_bin}/ember"
stage_custom_install="$(
    install_dry_run "${stage_root}" \
        --prefix "${runtime_prefix}" --bin-dir "${stage_custom_bin}"
)"
stage_custom_uninstall="$(
    uninstall_dry_run "${stage_root}" \
        --prefix "${runtime_prefix}" --bin-dir "${stage_custom_bin}"
)"
assert_contains "DESTDIR custom-bin install" "${stage_custom_install}" \
    "Staged file: ${stage_root}${stage_custom_bin}/ember"
assert_contains "DESTDIR custom-bin launcher" "${stage_custom_install}" \
    "Exec=${stage_custom_bin}/ember"
assert_contains "DESTDIR custom-bin uninstall" "${stage_custom_uninstall}" \
    "${stage_root}${stage_custom_bin}/ember"

# A release archive or distro packager can use the same installer without a
# Rust toolchain. Exercise actual copies and modes in a private staging root,
# and prove that launcher paths never leak DESTDIR into runtime metadata.
prebuilt_dir="${TEST_ROOT}/prebuilt"
prebuilt_binary="${prebuilt_dir}/ember"
roundtrip_stage="${TEST_ROOT}/roundtrip-stage"
roundtrip_prefix='/opt/ember release \dir $'
roundtrip_bin="${roundtrip_prefix}/bin"
roundtrip_share="${roundtrip_prefix}/share"
app_id="io.github.beamiter.ember"
mkdir -p "${prebuilt_dir}"
printf '#!/bin/sh\nprintf "ember release fixture\\n"\n' >"${prebuilt_binary}"
chmod 0600 "${prebuilt_binary}"

roundtrip_install="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
        "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${roundtrip_prefix}" \
        2>&1
)"
assert_contains "prebuilt install selects fixture" "${roundtrip_install}" \
    "Using prebuilt ember binary: ${prebuilt_binary}"
assert_contains "prebuilt install skips staged cache refresh" "${roundtrip_install}" \
    "Staged install (DESTDIR set); skipping desktop cache refresh."

installed_binary="${roundtrip_stage}${roundtrip_bin}/ember"
installed_desktop="${roundtrip_stage}${roundtrip_share}/applications/${app_id}.desktop"
installed_metainfo="${roundtrip_stage}${roundtrip_share}/metainfo/${app_id}.metainfo.xml"
installed_svg="${roundtrip_stage}${roundtrip_share}/icons/hicolor/scalable/apps/${app_id}.svg"
installed_png_128="${roundtrip_stage}${roundtrip_share}/icons/hicolor/128x128/apps/${app_id}.png"
installed_png_256="${roundtrip_stage}${roundtrip_share}/icons/hicolor/256x256/apps/${app_id}.png"

for installed_file in \
    "${installed_binary}" \
    "${installed_desktop}" \
    "${installed_metainfo}" \
    "${installed_svg}" \
    "${installed_png_128}" \
    "${installed_png_256}"; do
    assert_regular_file "staged install output" "${installed_file}"
done
cmp -- "${prebuilt_binary}" "${installed_binary}" \
    || fail "staged binary differs from the prebuilt input"
assert_mode "staged binary" "${installed_binary}" 755
for public_file in \
    "${installed_desktop}" \
    "${installed_metainfo}" \
    "${installed_svg}" \
    "${installed_png_128}" \
    "${installed_png_256}"; do
    assert_mode "staged public resource" "${public_file}" 644
done

expected_exec='Exec="/opt/ember release \\\\dir \\$/bin/ember"'
[[ "$(grep -Fxc "${expected_exec}" "${installed_desktop}")" == 2 ]] \
    || fail "desktop main/action Exec paths do not match the runtime binary"
expected_try_exec='TryExec=/opt/ember release \\dir $/bin/ember'
grep -Fxq "${expected_try_exec}" "${installed_desktop}" \
    || fail "desktop TryExec path does not match the runtime binary"
if grep -Fq "${roundtrip_stage}" "${installed_desktop}"; then
    fail "desktop entry leaked DESTDIR into a runtime path"
fi
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "${installed_desktop}"
fi

env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${UNINSTALLER}" --prefix "${roundtrip_prefix}" >/dev/null
for removed_file in \
    "${installed_binary}" \
    "${installed_desktop}" \
    "${installed_metainfo}" \
    "${installed_svg}" \
    "${installed_png_128}" \
    "${installed_png_256}"; do
    assert_absent "staged uninstall target" "${removed_file}"
done
assert_regular_file "prebuilt source after uninstall" "${prebuilt_binary}"

# Atomic replacement must replace a destination symlink itself, never follow it
# and overwrite an unrelated target. It must also leave no staging artifacts.
symlink_victim="${TEST_ROOT}/must-not-change"
printf 'victim\n' >"${symlink_victim}"
mkdir -p "$(dirname -- "${installed_binary}")"
ln -s -- "${symlink_victim}" "${installed_binary}"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${roundtrip_prefix}" \
    --no-desktop >/dev/null
assert_regular_file "atomically replaced destination" "${installed_binary}"
[[ ! -L "${installed_binary}" ]] \
    || fail "binary install followed or retained the destination symlink"
[[ "$(<"${symlink_victim}")" == victim ]] \
    || fail "binary install overwrote the destination symlink target"
cmp -- "${prebuilt_binary}" "${installed_binary}" \
    || fail "atomically replaced binary differs from its source"
shopt -s nullglob
binary_temps=("${installed_binary}.install."*)
desktop_temps=("${roundtrip_stage}${roundtrip_share}/applications/.${app_id}.desktop.install."*)
shopt -u nullglob
(( ${#binary_temps[@]} == 0 )) || fail "binary install left temporary files"
(( ${#desktop_temps[@]} == 0 )) || fail "desktop install left temporary files"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${UNINSTALLER}" --prefix "${roundtrip_prefix}" >/dev/null

# The pinned descriptor must name a non-empty artifact. Fail before creating
# the staged binary directory so a typo cannot leave a partial installation.
empty_prebuilt="${prebuilt_dir}/empty-ember"
: >"${empty_prebuilt}"
empty_stage="${TEST_ROOT}/empty-stage"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${empty_stage}" \
    "${INSTALLER}" --binary "${empty_prebuilt}" --prefix /opt/empty-ember \
    --no-desktop >"${TEST_ROOT}/empty-prebuilt.log" 2>&1; then
    fail "installer accepted an empty prebuilt binary"
fi
assert_contains "empty prebuilt file diagnostic" \
    "$(<"${TEST_ROOT}/empty-prebuilt.log")" "prebuilt binary must not be empty"
assert_absent "empty prebuilt staged target" \
    "${empty_stage}/opt/empty-ember/bin/ember"

# Existing symlink ancestors are forbidden only inside a non-root DESTDIR.
# This catches staging escape attempts before the binary commit.
ancestor_stage="${TEST_ROOT}/ancestor-stage"
ancestor_victim="${TEST_ROOT}/ancestor-victim"
mkdir -p "${ancestor_stage}" "${ancestor_victim}"
ln -s -- "${ancestor_victim}" "${ancestor_stage}/opt"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${ancestor_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix /opt/escaped-ember \
    --no-desktop >"${TEST_ROOT}/ancestor.log" 2>&1; then
    fail "installer accepted a symbolic-link ancestor below DESTDIR"
fi
assert_contains "staging ancestor diagnostic" "$(<"${TEST_ROOT}/ancestor.log")" \
    "symbolic-link ancestor"
[[ -z "$(find "${ancestor_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "staging ancestor validation wrote outside DESTDIR"

# Public resources use the same atomic replacement rule as the binary: replace
# a hostile destination link itself and never touch its target.
resource_stage="${TEST_ROOT}/resource-stage"
resource_prefix="/opt/ember-resource"
resource_victim="${TEST_ROOT}/resource-victim"
resource_metainfo="${resource_stage}${resource_prefix}/share/metainfo/${app_id}.metainfo.xml"
printf 'resource victim\n' >"${resource_victim}"
mkdir -p "$(dirname -- "${resource_metainfo}")"
ln -s -- "${resource_victim}" "${resource_metainfo}"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${resource_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${resource_prefix}" >/dev/null
assert_regular_file "atomically replaced metainfo" "${resource_metainfo}"
[[ ! -L "${resource_metainfo}" ]] || fail "metainfo destination remained a symlink"
[[ "$(<"${resource_victim}")" == "resource victim" ]] \
    || fail "metainfo install overwrote a symlink target"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${resource_stage}" \
    "${UNINSTALLER}" --prefix "${resource_prefix}" >/dev/null

# Force a signal after the same-directory temporary has been populated but
# before rename. EXIT cleanup must remove the temp and leave the old target.
interrupt_tools="${TEST_ROOT}/interrupt-tools"
interrupt_stage="${TEST_ROOT}/interrupt-stage"
interrupt_prefix="/opt/ember-interrupt"
interrupt_binary="${interrupt_stage}${interrupt_prefix}/bin/ember"
mkdir -p "${interrupt_tools}" "$(dirname -- "${interrupt_binary}")"
printf 'old interrupt ember\n' >"${interrupt_binary}"
# The single-quoted parameter references belong to the generated helper.
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=""' \
    'for argument do last="${argument}"; done' \
    '/usr/bin/install "$@"' \
    'case "${last}" in *.install.*) kill -TERM "${PPID}" ;; esac' \
    >"${interrupt_tools}/install"
chmod 0755 "${interrupt_tools}/install"
if {
    env HOME="${TEST_HOME}" PATH="${interrupt_tools}:${TEST_PATH}" \
        DESTDIR="${interrupt_stage}" "${INSTALLER}" --binary "${prebuilt_binary}" \
        --prefix "${interrupt_prefix}" --no-desktop
} >"${TEST_ROOT}/interrupt.log" 2>&1; then
    fail "interrupted installer unexpectedly succeeded"
fi
[[ "$(<"${interrupt_binary}")" == 'old interrupt ember' ]] \
    || fail "pre-rename interruption replaced the old binary"
shopt -s nullglob
interrupt_temps=("${interrupt_binary}.install."*)
shopt -u nullglob
(( ${#interrupt_temps[@]} == 0 )) \
    || fail "pre-rename interruption left a binary temporary"

prebuilt_symlink="${prebuilt_dir}/ember-link"
ln -s -- "${prebuilt_binary}" "${prebuilt_symlink}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${INSTALLER}" --binary "${prebuilt_symlink}" --prefix "${roundtrip_prefix}" \
    >"${TEST_ROOT}/symlink-binary.log" 2>&1; then
    fail "installer accepted a symlinked prebuilt binary"
fi
assert_contains "symlinked prebuilt diagnostic" \
    "$(<"${TEST_ROOT}/symlink-binary.log")" \
    "prebuilt binary must not be a symbolic link: ${prebuilt_symlink}"
assert_regular_file "symlink target after rejection" "${prebuilt_binary}"

missing_binary="${TEST_ROOT}/does-not-exist/ember"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${INSTALLER}" --binary "${missing_binary}" --prefix "${roundtrip_prefix}" \
    >"${TEST_ROOT}/missing-binary.log" 2>&1; then
    fail "installer accepted a missing prebuilt binary"
fi
assert_contains "missing prebuilt diagnostic" \
    "$(<"${TEST_ROOT}/missing-binary.log")" \
    "prebuilt binary is not a regular file: ${missing_binary}"

if install_dry_run "" --binary= >"${TEST_ROOT}/empty-binary.log" 2>&1; then
    fail "installer accepted an empty --binary path"
fi
assert_contains "empty prebuilt diagnostic" \
    "$(<"${TEST_ROOT}/empty-binary.log")" \
    "--binary must not be empty"

if install_dry_run "" --prefix '/opt/ember=invalid' \
    >"${TEST_ROOT}/invalid-desktop-path.log" 2>&1; then
    fail "installer accepted '=' in a desktop executable path"
fi
assert_contains "invalid desktop executable diagnostic" \
    "$(<"${TEST_ROOT}/invalid-desktop-path.log")" \
    "desktop executable path must not contain '='"

if install_dry_run "" --prefix '/opt/ember%invalid' \
    >"${TEST_ROOT}/percent-desktop-path.log" 2>&1; then
    fail "installer accepted '%' in a desktop executable path"
fi
assert_contains "invalid percent executable diagnostic" \
    "$(<"${TEST_ROOT}/percent-desktop-path.log")" \
    "desktop executable path must not contain '%'"

invalid_stage="${TEST_ROOT}/invalid-desktop-stage"
invalid_prefix='/opt/ember=invalid'
sentinel_binary="${invalid_stage}${invalid_prefix}/bin/ember"
mkdir -p "$(dirname -- "${sentinel_binary}")"
printf 'old ember\n' >"${sentinel_binary}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${invalid_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${invalid_prefix}" \
    >"${TEST_ROOT}/desktop-preflight.log" 2>&1; then
    fail "installer accepted an invalid desktop executable path"
fi
[[ "$(<"${sentinel_binary}")" == 'old ember' ]] \
    || fail "desktop preflight failure replaced the old binary"

for bad_path in '/opt/ember/../escape' '/opt/ember/'$'bad\npath'; do
    if install_dry_run "${stage_root}" --prefix "${bad_path}" \
        >"${TEST_ROOT}/bad-install-path.log" 2>&1; then
        fail "installer accepted unsafe prefix ${bad_path@Q}"
    fi
    if uninstall_dry_run "${stage_root}" --prefix "${bad_path}" \
        >"${TEST_ROOT}/bad-uninstall-path.log" 2>&1; then
        fail "uninstaller accepted unsafe prefix ${bad_path@Q}"
    fi
done
assert_contains "parent path diagnostic" \
    "$(install_dry_run "${stage_root}" --prefix '/opt/ember/../escape' 2>&1 || true)" \
    "--prefix must not contain '..' path components"

if install_dry_run "${stage_root}/../escape" --prefix /opt/ember \
    >"${TEST_ROOT}/bad-destdir.log" 2>&1; then
    fail "installer accepted a DESTDIR with a parent component"
fi
assert_contains "DESTDIR parent diagnostic" "$(<"${TEST_ROOT}/bad-destdir.log")" \
    "DESTDIR must not contain '..' path components"

for command in install_dry_run uninstall_dry_run; do
    if "${command}" "" --bin-dir= >"${TEST_ROOT}/empty-bin.log" 2>&1; then
        fail "${command} accepted an empty --bin-dir"
    fi
    assert_contains "empty bin diagnostic" "$(<"${TEST_ROOT}/empty-bin.log")" \
        "--bin-dir must not be empty"
done

# `/` is a meaningful, explicitly supplied staging root even though removing
# its trailing slash yields an empty concatenation prefix.
root_stage_install="$(install_dry_run / --prefix /opt/ember-root)"
assert_contains "root DESTDIR cache policy" "${root_stage_install}" \
    "Staged install (DESTDIR set); skipping desktop cache refresh."
assert_contains "root DESTDIR summary" "${root_stage_install}" \
    "Staged file: /opt/ember-root/bin/ember"

# Lexically harmless dot components and Unicode remain valid path spellings.
portable_path='/opt/./火焰 terminal'
assert_contains "Unicode path accepted" \
    "$(install_dry_run "${stage_root}" --prefix "${portable_path}")" \
    "Installed ember to ${portable_path}/bin/ember"

uninstall_link_stage="${TEST_ROOT}/uninstall-link-stage"
uninstall_link_victim="${TEST_ROOT}/uninstall-link-victim"
uninstall_link_prefix="/opt/ember-uninstall-link"
mkdir -p "${uninstall_link_stage}" \
    "${uninstall_link_victim}/ember-uninstall-link/bin"
printf 'outside ember\n' \
    >"${uninstall_link_victim}/ember-uninstall-link/bin/ember"
ln -s -- "${uninstall_link_victim}" "${uninstall_link_stage}/opt"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" \
    DESTDIR="${uninstall_link_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_link_prefix}" >"${TEST_ROOT}/uninstall-link.log" 2>&1; then
    fail "uninstaller followed a symbolic-link ancestor below DESTDIR"
fi
assert_contains "uninstall ancestor diagnostic" \
    "$(<"${TEST_ROOT}/uninstall-link.log")" \
    "staged uninstall path contains a symbolic-link ancestor"
[[ "$(<"${uninstall_link_victim}/ember-uninstall-link/bin/ember")" == \
    'outside ember' ]] || fail "uninstaller removed a file outside DESTDIR"

# A symlinked DESTDIR root used to evade `-L` when spelled as `link/.` or with
# repeated trailing separators. Normalize that spelling and inspect the full
# existing root chain before install or uninstall can touch the victim.
root_link="${TEST_ROOT}/destdir-root-link"
root_victim="${TEST_ROOT}/destdir-root-victim"
root_prefix="/opt/ember-destdir-root"
root_binary="${root_victim}${root_prefix}/bin/ember"
mkdir -p "${root_victim}"
ln -s -- "${root_victim}" "${root_link}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${root_link}/." \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${root_prefix}" \
    --no-desktop >"${TEST_ROOT}/root-link-install.log" 2>&1; then
    fail "installer accepted a symlinked DESTDIR root disguised with /."
fi
assert_contains "symlinked DESTDIR install diagnostic" \
    "$(<"${TEST_ROOT}/root-link-install.log")" \
    "DESTDIR path contains a symbolic-link component"
[[ -z "$(find "${root_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "symlinked DESTDIR install wrote outside its staging boundary"

mkdir -p "$(dirname -- "${root_binary}")"
printf 'outside root ember\n' >"${root_binary}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${root_link}//" \
    "${UNINSTALLER}" --prefix "${root_prefix}" \
    >"${TEST_ROOT}/root-link-uninstall.log" 2>&1; then
    fail "uninstaller accepted a symlinked DESTDIR root with trailing separators"
fi
assert_contains "symlinked DESTDIR uninstall diagnostic" \
    "$(<"${TEST_ROOT}/root-link-uninstall.log")" \
    "DESTDIR path contains a symbolic-link component"
[[ "$(<"${root_binary}")" == 'outside root ember' ]] \
    || fail "symlinked DESTDIR uninstall removed an outside binary"

printf 'install/uninstall path contract: ok\n'
