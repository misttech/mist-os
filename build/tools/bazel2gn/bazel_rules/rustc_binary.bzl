# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("@rules_rust//rust:defs.bzl", "rust_binary", "rust_test")

# rustc_binary optionally defines a test target for the binary target.
#
# Besides being a shorthand, this is mainly used to allow easier syncing between
# Bazel and GN targets. See details in http://fxbug.dev/407441714.
def rustc_binary(name, with_unit_tests = False, test_deps = [], **kwargs):
    rust_binary(
        name = name,
        **kwargs
    )
    if with_unit_tests:
        rust_test(
            name = "{}_test".format(name),
            crate = ":{}".format(name),
            deps = test_deps,
        )
