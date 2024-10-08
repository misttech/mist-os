#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script transforms .rlib arguments into .rmeta.
# This helps drive tools that only require rmetas instead of full rlibs.
#
# For https://fxbug.dev/370572079, for example, the rustdoc tool
# only requires rmeta files, which saves bandwidth by avoiding
# downloading full rlibs from remote builds.

input="$1"
output="$2"

exec sed -e 's|\.rlib\>|.rmeta|g' < "$input" > "$output"

