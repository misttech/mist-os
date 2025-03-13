#!/bin/sh
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
set -e

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"/..
readonly REPO_ROOT="${REPO_ROOT}"

# Runs our bootstrap script to ensure we have bazel
run_bootstrap() {
  "${REPO_ROOT}/scripts/bootstrap.sh"
}

# Make sure we have a build config file at the root of out repo.
ensure_build_config_file() {
  cd "${REPO_ROOT}"

  # Write out the build directory to be used by our config
  tools/bazel info output_base > .build-dir

  # Write out our build config entry.
  tools/bazel run \
    --run_under="cd $PWD && " \
    --ui_event_filters=-info,-error,-debug,-stderr,-stdout \
    @rules_fuchsia//fuchsia/tools:ensure_build_config -- \
      --config-file ".fuchsia-build-config.json"
}

main() {
  run_bootstrap
  ensure_build_config_file
}

main "$@"
