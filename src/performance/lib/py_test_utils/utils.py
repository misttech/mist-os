# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Provides utilities for Python performance tests defined with python_perf_test.gni.
"""

import os


def get_associated_runtime_deps_dir(search_dir: os.PathLike) -> os.PathLike:
    """Return the directory that contains runtime dependencies.

    Args:
      search_dir: Absolute path to directory where runtime_deps dir is an
        ancestor of.

    Returns: Path to runtime_deps directory
    """
    cur_path: str = os.path.dirname(search_dir)
    while not os.path.isdir(os.path.join(cur_path, "runtime_deps")):
        cur_path = os.path.dirname(cur_path)
        if cur_path == "/":
            raise ValueError("Couldn't find required runtime_deps directory")
    return os.path.join(cur_path, "runtime_deps")
