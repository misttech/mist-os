#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script is expected to be invoked as a Jiri hook from
# integration/fuchsia/stem.

# Its purpose is to generate a Unix timestamp and a commit hash
# file under //build/info/jiri_generated/
#
# For context, see https://fxbug.dev/335391299
#
# This uses the //build/info/gen_latest_commit_date.py script
# with the --force-git option to generate them. Without this
# option, the same script, which will be invoked at build time,
# will read these files as input instead, and process them before
# writing them to their final destination in the Ninja or Bazel
# build artifact directories.

_SCRIPT_DIR="$(dirname "${BASH_SOURCE[0]}")"

fatal () {
  echo >&2 "FATAL: $*"
  exit 1
}

# Assume this script lives under //build/info/
FUCHSIA_DIR="$(cd "${_SCRIPT_DIR}/../.." && pwd -P 2>/dev/null)"
if [[ ! -f "${FUCHSIA_DIR}/.jiri_manifest" ]]; then
  fatal "Cannot locate proper FUCHSIA_DIR, got: ${FUCHSIA_DIR}"
fi

# Use the OSTYPE and MACHTYPE Bash builtin variables to determine host
# machine type.
case "$OSTYPE" in
  linux*)
    readonly HOST_OS="linux"
    ;;
  darwin*)
    readonly HOST_OS="mac"
    ;;
  *)
    echo >&2 "Unknown operating system: $OSTYPE."
    exit 1
    ;;
esac

case "$MACHTYPE" in
  x86_64*)
    readonly HOST_CPU="x64"
    ;;
  aarch64*|arm64*)
    readonly HOST_CPU="arm64"
    ;;
  *)
    echo >&2 "Unknown architecture: $MACHTYPE."
    exit 1
    ;;
esac

# Locate prebuilt python interpreter.
PREBUILT_PYTHON="$FUCHSIA_DIR/prebuilt/third_party/python3/${HOST_OS}-${HOST_CPU}/bin/python3"
if [[ ! -f "${PREBUILT_PYTHON}" ]]; then
  fatal "Cannot locate PREBUILT_PYTHON: $PREBUILT_PYTHON"
fi

# LINT.IfChange
OUTPUT_DIR="${_SCRIPT_DIR}/jiri_generated"
mkdir -p "${OUTPUT_DIR}"

"$PREBUILT_PYTHON" -S \
  "${_SCRIPT_DIR}/gen_latest_commit_date.py" \
  --repo "${FUCHSIA_DIR}/integration" \
  --timestamp-file "$OUTPUT_DIR/integration_commit_stamp.txt" \
  --commit-hash-file "${OUTPUT_DIR}/integration_commit_hash.txt"
# LINT.ThenChange(//build/info/gen_latest_commit_date.py)
