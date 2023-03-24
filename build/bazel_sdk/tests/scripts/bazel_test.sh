#!/bin/bash

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

main() {
  local fuchsia_build_dir=$(fx get-build-dir)
  echo
  echo "Setting LOCAL_FUCHSIA_PLATFORM_BUILD to ${fuchsia_build_dir}"
  echo
  export LOCAL_FUCHSIA_PLATFORM_BUILD=${fuchsia_build_dir}
  (
    cd $(dirname "$0")/..
    bazel test :tests
  )
}

main "$@"
