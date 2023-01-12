# Copyright 2017 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Return the top-directory of a given Bazel workspace used by the platform
# build. A TOPDIR contains several files and directories like workspace/
# or output_base/
#
fx-bazel-top-dir () {
  # See //build/bazel/config/README.md
  #
  # $1: Optional workspace name, defaults to "main".
  #
  local INPUT_FILE="${FUCHSIA_DIR}/build/bazel/config/${1:-main}_workspace_top_dir"
  local TOPDIR=$(cat "${INPUT_FILE}")
  echo "${FUCHSIA_BUILD_DIR}/${TOPDIR}"
}
