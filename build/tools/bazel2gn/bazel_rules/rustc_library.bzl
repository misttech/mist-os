# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("@rules_rust//rust:defs.bzl", "rust_library", "rust_test")

# rustc_library optionally defines a test target for the library target.
#
# Besides being a shorthand, this is mainly used to allow easier syncing between
# Bazel and GN targets. See details in http://fxbug.dev/407441714.
def rustc_library(name, with_unit_tests = False, test_deps = [], **kwargs):
    rust_library(
        name = name,
        **kwargs
    )
    if with_unit_tests:
        rust_test(
            name = "{}_test".format(name),
            crate = ":{}".format(name),
            deps = test_deps,
            crate_features = kwargs.get("crate_features", []),
        )
