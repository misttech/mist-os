# Copyright 2017 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# shellcheck disable=SC2148
# No shebang in this file is intentional

# Return the top-directory of a given Bazel workspace used by the platform
# build. A TOPDIR contains several files and directories like workspace/
# or output_base/
fx-bazel-top-dir () {
  # See //build/bazel/config/README.md
  local INPUT_FILE="${FUCHSIA_DIR}/build/bazel/config/main_workspace_top_dir"
  local TOPDIR
  TOPDIR=$(<"${INPUT_FILE}")
  echo "${FUCHSIA_BUILD_DIR}/${TOPDIR}"
}

# Return path to Bazel workspace.
fx-get-bazel-workspace () {
  printf %s/workspace "$(fx-bazel-top-dir)"
}

# Return the path to the Bazel wrapper script.
fx-get-bazel () {
   printf %s/bazel "$(fx-bazel-top-dir)"
}

# Regenerate Bazel workspace and launcher script if needed.
# Note that this also regenerates the Ninja build plan if necessary.
fx-update-bazel-workspace () {
  # First, refresh Ninja build plan if needed.
  local check_script="${FUCHSIA_DIR}/build/bazel/scripts/check_regenerator_inputs.py"
  if ! "${PREBUILT_PYTHON3}" -S "${check_script}" --quiet "${FUCHSIA_BUILD_DIR}"; then
    echo "fx-bazel: Regenerating workspace due to input file changes!"
    fx-command-run gen
  fi
}

# Run bazel command in the Fuchsia workspace, after ensuring it is up-to-date.
fx-bazel () {
  fx-update-bazel-workspace
  "$(fx-get-bazel)" "$@"
}
