#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
List clean build fences. Every line should include a reference to a bug which justifies the
addition.

WARNING: Exercise great caution when adding to this file, as the consequences are
significant and widespread. Every Fuchsia developer and builder will have their incremental
build cache invalidated when receiving or reverting the change to do so. Only add to this
file after consulting with the Build team about failed attempts to address build convergence
issues within the dependency graph.
"""

import sys


def print_fences():
    """
    All fences are emitted from here.
    """
    print(
        "ninja complains about a cyclic dependency in //examples/components/config/integration_test (https://fxbug.dev/42180109)"
    )
    print(
        "ninja complains about a cyclic dependency in //src/virtualization/bin/vmm/device/virtio_net/virtio_net (https://fxbug.dev/42066177)"
    )
    print(
        "After fxr/829176, assembly complains that host_tools.modular manifest cannot be found (https://fxbug.dev/42075721)."
    )
    print(
        "After fxr/898958, assembly complains about fshost equivalence in zedboot even though zedboot should not be built in user/userdebug."
    )
    print(
        "After fxr/973216, Bazel build complains about dangling symlinks, see http://b/319069000#comment4"
    )
    print(
        "After fxr/1081759, Bazel build is non-incremental, see http://b/353592055"
    )
    print(
        "fxr/1098532 triggers a Bazel error where SDK header changes don't trigger rebuilds, see http://b/356347441"
    )


def main():
    print_fences()
    return 0


if __name__ == "__main__":
    sys.exit(main())
