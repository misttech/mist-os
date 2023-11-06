#!/bin/sh

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This scripts asserts product bundle names are unique.

# Usage:
#
# $0 jq_path product_bundles_manifest_path stamp_file_path

set -e

readonly jq=$1
readonly pb_manifest=$2
readonly stamp_file=$3

duplicates=($(${jq} --raw-output 'group_by(.name) | map(select(length > 1) | .[0].name) | .[]' "${pb_manifest}"))
if (( ${#duplicates[@]} )); then
  echo "Found product bundles in ${pb_manifest} with duplicate names: ${duplicates[@]}."
  exit 1
fi
touch "${stamp_file}"
