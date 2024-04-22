#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script is used to manually check that the logic used
# to generate integration commit hash / stamp / date files is
# consistent between GN and Bazel, and works either with or without
# Jiri hook generated files under build/info/jiri_generated/
#
# This cannot be run as a build action, as it invokes `fx build`
# and `fx bazel build`, but can be run locally by a developer if
# something afoul is suspected.
#
# It verifies that everything works under three conditions:
#
#  1) When there are no hook files under //build/info/jiri_generated/,
#     which should be the case before tqr/832170 is submitted.
#
#  2) When there are files under //build/info/jiri_generated/ with
#     fake values, to verify that they are correctly picked up in
#     the GN and Bazel output artifacts.
#
#  3) When the files under //build/info/jiri_generated/ correspond to
#     the result of running //build/info/create_jiri_hook_files.sh, which
#     should be invoked by a Jiri hook, to verify that the GN and Bazel
#     output artifacts match their content.
#
#  4) When the files are removed, which simulates disabling the
#     Jiri hook (e.g. in the case of a revert), to verify that the depfile
#     no longer references the missing files.
#

set -e

fx build bazel_workspace
BAZEL_OUTPUT_BASE=$(fx bazel info output_base)

FAILURE=0

compare_files () {
  diff -qb "$1" "$2"
}

# Verify that the content of two files is identical.
# $1: first file path
# $2: second file path
# $3: files description text.
assert_files_equal () {
  local file_a="$1"
  local file_b="$2"
  local description="$3"

  if compare_files "${file_a}" "${file_b}"; then
    echo "OK: ${description}"
  else
    echo "KO: ${description}: [$(cat "${file_a}")] != [$(cat "${file_b}")]";
    FAILURE=1
    return 1
  fi
}

# $1: Ninja target file.
# $2+: depfile input dependencies.
assert_depfile_equals () {
  local ninja_target="$1"
  shift
  local expected=("$@")
  fx build -- -t deps "${ninja_target}" > "$TMPDIR/depfile"
  # Example depfile:
  #
  # minimum-utc-stamp.txt: #deps 2, deps mtime 1713788552843776809 (VALID)
  #  ../../build/info/jiri_generated/integration_commit_hash.txt
  #  ../../build/info/jiri_generated/integration_commit_stamp.txt
  #
  # Remove first line, empty lines, then extract content to array variable.
  local -a actual
  actual=($(sed -e '1d'  -e '/^$/d' "$TMPDIR/depfile"))
  if [[ "${actual[*]}" == "${expected[*]}" ]]; then
    echo "OK: ${ninja_target} depfile"
  else
    printf "KO: %s depfile:\nEXPECTED: [%s]\nACTUAL  : [%s]\n" "${ninja_target}" "${expected[*]}" "${actual[*]}"
    FAILURE=1
  fi
}

declare -a HOOK_FILES=(
  build/info/jiri_generated/integration_commit_hash.txt
  build/info/jiri_generated/integration_commit_stamp.txt
)

TMPDIR=/tmp/check-jiri-hooks
mkdir -p "${TMPDIR}"

export GIT_OPTIONAL_LOCKS=0

INTEGRATION_HASH="$TMPDIR/integration_hash.txt"
INTEGRATION_STAMP="$TMPDIR/integration_stamp.txt"
git -C integration --no-optional-locks rev-parse HEAD > "${INTEGRATION_HASH}"
git -C integration --no-optional-locks log -n1 --date=unix --format=%cd > "${INTEGRATION_STAMP}"
echo "Current integration hash: $(cat "${INTEGRATION_HASH}")"
echo "Current integration stamp: $(cat "${INTEGRATION_STAMP}")"

save_hook_files () {
  for HOOK_FILE in "${HOOK_FILES[@]}"; do
    local DST_FILE="$TMPDIR/${HOOK_FILE}"
    mkdir -p "$(dirname "${DST_FILE}")"
    if [[ -e "${HOOK_FILE}" ]]; then
      cp -f "${HOOK_FILE}" "${DST_FILE}"
    else
      rm -f "${DST_FILE}"
    fi
  done
}

restore_hook_files () {
  for HOOK_FILE in "${HOOK_FILES[@]}"; do
    local SRC_FILE="$TMPDIR/${HOOK_FILE}"
    if [[ -e "${SRC_FILE}" ]]; then
      cp -f "${SRC_FILE}" "${HOOK_FILE}"
    else
      rm -f "${HOOK_FILE}"
    fi
  done
}

cleanup () {
  restore_hook_files
  rm -rf "${TMPDIR}"
}

trap cleanup EXIT SIGINT SIGHUP SIGTERM


do_gn_build () {
  fx build //build/info:build_info_files -- --quiet
}

force_gn_build () {
  touch build/info/gen_latest_commit_date.py
  do_gn_build
}

do_bazel_build () {
  fx bazel build --config=quiet //build/info:latest_date_and_timestamp
}

force_bazel_build () {
  # This ensures that @fuchsia_build_info//:args.bzl is
  # re-computed, since its `available_jiri_hook_files`
  # dictionary depends on the current state of
  # //build/info/jiri_generated/, and is used by the
  # rule defining the target.
  rm -rf "${BAZEL_OUTPUT_BASE}/external/fuchsia_build_info"
  do_bazel_build
}

# Saving existing hook files, if they exist, then removing them
save_hook_files

echo "###########################################################"
echo "###########################################################"
echo "#####"
echo "#####    TEST WITH NO HOOK FILES"
echo "#####"

rm -f "${HOOK_FILES[@]}"
force_gn_build
assert_files_equal \
    out/default/latest-commit-hash.txt \
    "${INTEGRATION_HASH}" \
    "GN Hash (no hook files)"
assert_files_equal \
    out/default/minimum-utc-stamp.txt \
    "${INTEGRATION_STAMP}" \
    "GN Stamp (no hook files)"
assert_depfile_equals \
    minimum-utc-stamp.txt \
    "../../integration/.git/HEAD"

force_bazel_build
assert_files_equal \
    out/default/gen/build/bazel/workspace/bazel-bin/build/info/latest_commit_hash.txt \
    "${INTEGRATION_HASH}" \
    "Bazel Hash (no hook files)"
assert_files_equal \
    out/default/gen/build/bazel/workspace/bazel-bin/build/info/minimum_utc_stamp.txt \
    "${INTEGRATION_STAMP}" \
    "Bazel Stamp (no hook files)"

echo "###########################################################"
echo "###########################################################"
echo "#####"
echo "#####    TEST WITH FAKE HOOK FILES"
echo "#####"

# Create fake hook file values to ensure they are picked up correctly.
FAKE_HASH=/tmp/fake_hash.txt
FAKE_STAMP=/tmp/fake_stamp.txt
cat > "$FAKE_HASH" <<< "thisisafakehash"
cat > "$FAKE_STAMP" <<< "42"

cp "${FAKE_HASH}" "${HOOK_FILES[0]}"
cp "${FAKE_STAMP}" "${HOOK_FILES[1]}"

force_gn_build
assert_files_equal \
    out/default/latest-commit-hash.txt \
    "${FAKE_HASH}" \
    "GN Hash (fake hook files)"

assert_files_equal \
    out/default/minimum-utc-stamp.txt \
    "${FAKE_STAMP}" \
    "GN Stamp (fake hook files)"

assert_depfile_equals \
    minimum-utc-stamp.txt \
    "../../build/info/jiri_generated/integration_commit_hash.txt" \
    "../../build/info/jiri_generated/integration_commit_stamp.txt" \

force_bazel_build
assert_files_equal \
    out/default/gen/build/bazel/workspace/bazel-bin/build/info/latest_commit_hash.txt \
    "${FAKE_HASH}" \
    "Bazel Hash (fake hook files)"

assert_files_equal \
    out/default/gen/build/bazel/workspace/bazel-bin/build/info/minimum_utc_stamp.txt \
    "${FAKE_STAMP}" \
    "Bazel Stamp (fake hook files)"

echo "###########################################################"
echo "###########################################################"
echo "#####"
echo "#####    TEST WITH REAL HOOK FILES"
echo "#####"

# Now run the Jiri hook script explicitly.
echo "Invoking hook script."
build/info/create_jiri_hook_files.sh
assert_files_equal \
    "${HOOK_FILES[0]}" \
    "${INTEGRATION_HASH}" \
    "Hook generated hash file."
assert_files_equal \
    "${HOOK_FILES[1]}" \
    "${INTEGRATION_STAMP}" \
    "Hook generated stamp file."

force_gn_build
assert_files_equal \
    out/default/latest-commit-hash.txt \
    "${INTEGRATION_HASH}" \
    "GN Hash (real hook files)"
assert_files_equal \
    out/default/minimum-utc-stamp.txt \
    "${INTEGRATION_STAMP}" \
    "GN Stamp (no hook files)"

assert_depfile_equals \
    minimum-utc-stamp.txt \
    "../../build/info/jiri_generated/integration_commit_hash.txt" \
    "../../build/info/jiri_generated/integration_commit_stamp.txt" \

force_bazel_build
assert_files_equal \
    out/default/gen/build/bazel/workspace/bazel-bin/build/info/latest_commit_hash.txt \
    "${INTEGRATION_HASH}" \
    "Bazel Hash (real hook files)"
assert_files_equal \
    out/default/gen/build/bazel/workspace/bazel-bin/build/info/minimum_utc_stamp.txt \
    "${INTEGRATION_STAMP}" \
    "Bazel Stamp (real hook files)"

echo "###########################################################"
echo "###########################################################"
echo "#####"
echo "#####    TEST WITH REMOVED HOOK FILES"
echo "#####"
rm -f "${HOOK_FILES[@]}"
do_gn_build
assert_files_equal \
    out/default/latest-commit-hash.txt \
    "${INTEGRATION_HASH}" \
    "GN Hash (no hook files)"
assert_files_equal \
    out/default/minimum-utc-stamp.txt \
    "${INTEGRATION_STAMP}" \
    "GN Stamp (no hook files)"
assert_depfile_equals \
    minimum-utc-stamp.txt \
    "../../integration/.git/HEAD"


if [[ "${FAILURE}" != "0" ]]; then
  echo >&2 "KO: Some tests failed."
else
  echo "OK: All tests passed."
fi
