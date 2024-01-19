#!/bin/sh
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script creates a shim in the root build directory that runs a relative
# path supplied as this script's first argument. The path should be relative
# to the root build directory or system-absolute. The shim has the same name as
# the binary it runs.

set -e

tool_path="$1"
tool_name="$(basename "$1")"

{
    echo "#!/bin/sh"
    case "$tool_path" in
        # Absolute path.
        /*)
            echo 'exec "'${tool_path}'"' '"$@"'
        ;;
        # Relative to build dir (where this script lives). Accommodate symlinks.
        *)
            echo 'exec $(dirname $(readlink -f "$0"))"'/${tool_path}'"' '"$@"'
        ;;
    esac
} >"$tool_name"
chmod +x "$tool_name"
