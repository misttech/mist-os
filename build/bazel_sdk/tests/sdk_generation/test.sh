#!/bin/bash

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This test is implemented as a shell script temporarily, but it should be moved
# to Bazel code as soon as possible.

set -e


TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cd "${TEST_DIR}/.."

scripts/bootstrap.sh

tools/bazel build @fuchsia_sdk_both//:ffx @fuchsia_sdk_x64_only//:ffx @fuchsia_sdk_arm64_only//:ffx

dir_both="$(mktemp -d)"
dir_merged="$(mktemp -d)"

# copy both workspaces, one for each architecture, on top of each other
cp -R --dereference bazel-tests/external/fuchsia_sdk_x64_only/* "$dir_merged"
cp -R --dereference bazel-tests/external/fuchsia_sdk_arm64_only/* "$dir_merged"


# copy the workspace that is created from the multi-architecture IDK
cp -R --dereference bazel-tests/external/fuchsia_sdk_both/* "$dir_both"


# The only expected difference between a merged SDK and one created with multi-arch IDKs
# should be the meta/manifest.json file, which has a field for target
# architectures. To avoid a false result in diff, we change the specific line
# that we expect to be different to be the same:

sed -i''  's|"arm64"|"x64", "arm64"|' "${dir_merged}/meta/manifest.json"

echo "Comparing merged Bazel SDK ($dir_merged) with Bazel SDK produced from a multi-architecture IDK ($dir_both)..."
diff -r "$dir_merged" "$dir_both"
echo "SUCCESS! Multiple Bazel SDK for different architectures can be merged by simple directory merging operations."
