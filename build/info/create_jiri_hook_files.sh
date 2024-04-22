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

# LINT.IfChange
# Call git directly, as Python is not available when Jiri hooks run on infra bots.
export GIT_OPTIONAL_LOCKS=0
git -C "${FUCHSIA_DIR}/integration" rev-parse HEAD > "${FUCHSIA_DIR}/build/info/jiri_generated/integration_commit_hash.txt"
git -C "${FUCHSIA_DIR}/integration" log -n1 --date=unix --format=%cd > "${FUCHSIA_DIR}/build/info/jiri_generated/integration_commit_stamp.txt"
# LINT.ThenChange(//build/info/gen_latest_commit_date.py)
