#!/bin/bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Common dispatcher for standalone python binaries and tests,
# that need an adjusted PYTHONPATH to point to compiled python pb code.
# To use this script, symlink a .sh to this script, using the python script's
# basename.

script="$0"
# 'stem' is any executable python binary or test
stem="$(basename "$script" .sh)"
script_dir="$(dirname "$script")"

source "$script_dir"/common-setup.sh
# 'python' is defined

script_dir_abs="$(normalize_path "$script_dir")"
project_root="$default_project_root"

test -f "$script_dir"/proto/api/proxy/log_pb2.py || {
  cat <<EOF
Generated source $script_dir/proto/api/proxy/log_pb2.py not found.
Run $script_dir/proto/refresh.sh first.
EOF
  exit 1
}

env \
  PYTHONPATH="$script_dir_abs":"$script_dir_abs"/proto:"$project_root"/third_party/protobuf/python \
  "$python" \
  -S \
  "$script_dir"/"$stem".py \
  "$@"
