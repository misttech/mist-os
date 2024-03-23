# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Public definitions for the foo library"""

load(
    "//tools/bazel-docgen/testdata/foo:foo_internal.bzl",
    _emtpy = "empty",
)

empty = _emtpy
