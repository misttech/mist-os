#!/bin/sh

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This scripts asserts there's one and only one product bundle included in the
# input product bundles manifest.

# Usage:
#
# $0 jq_path product_bundles_manifest_path stamp_file_path

set -e

readonly jq=$1
readonly pb_manifest=$2
readonly stamp_file=$3

pb_count="$(${jq} '. | length' ${pb_manifest})"
if [[ "${pb_count}" -ne 1 ]]; then
  echo "Expecting exactly 1 product bundle from $1, got ${pb_count}."
  exit 1
fi
touch "${stamp_file}"
