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

  # The Bazel workspace assumes that the Fuchsia cpu is the host
  # CPU unless --cpu or --platforms is used. Extract the target_cpu
  # from ${fuchsia_build_dir}/args.json and construct the corresponding
  # bazel test argument.
  local target_cpu=$(fx jq --raw-output .target_cpu "${fuchsia_build_dir}"/args.json)

  (
    cd $(dirname "$0")/..
    bazel test --config=fuchsia_${target_cpu} :tests "$@"
  )
}

main "$@"
