#!/bin/bash
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# See https://github.com/bazelbuild/remote-apis-sdks
# `remotetool` can be used to inspect past remote actions, re-execute them,
# fetch artifacts, etc.

set -uo pipefail

script="$0"
stem="$(basename "$script" .sh)"
script_dir="$(dirname "$script")"

source "$script_dir"/common-setup.sh
# 'python' is defined

exec "$python" -S "$script_dir"/"$stem".py "$@"
