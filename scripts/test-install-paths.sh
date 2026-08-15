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

printf 'install/uninstall path contract: ok\n'
