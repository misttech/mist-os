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

# Update the target of an existing symlink.
# $1: symlink path.
# $2: new target path.
_update_symlink () {
  local symlink=$1
  local new_target=$2
  local current_target
  current_target="$(readlink "${symlink}")"
  if [[ "${current_target}" != "${new_target}" ]]; then
    rm "${symlink}"
    ln -s "${new_target}" "${symlink}"
  fi
}

# Regenerate Bazel workspace and launcher script if needed.
# Note that this also regenerates the Ninja build plan if necessary.
# $1: Workspace type, either "fuchsia" or "no_sdk"
fx-update-bazel-workspace () {
  # First, refresh Ninja build plan if needed.
  local check_script="${FUCHSIA_DIR}/build/bazel/scripts/check_regenerator_inputs.py"
  if ! "${PREBUILT_PYTHON3}" -S "${check_script}" --quiet "${FUCHSIA_BUILD_DIR}"; then
    echo "fx-bazel: Regenerating workspace due to input file changes!"
    fx-command-run gen
  fi

  # Update the bazel_root_files symlink to point to the directory holding
  # the right files for this variant of the workspace.
  local workspace_dir
  workspace_dir="$(fx-get-bazel-workspace)"
  _update_symlink \
    "${workspace_dir}/fuchsia_build_generated/bazel_root_files" \
    "bazel_root_files.$1"
}

# Run bazel command in the Fuchsia workspace, after ensuring it is up-to-date.
fx-bazel () {
  local workspace_variant="fuchsia"
  local opt
  fx-update-bazel-workspace "${workspace_variant}"
  "$(fx-get-bazel)" "$@"
}
