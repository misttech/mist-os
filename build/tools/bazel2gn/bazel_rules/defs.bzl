# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("//build/tools/bazel2gn/bazel_rules:rustc_binary.bzl", _rustc_binary = "rustc_binary")
load("//build/tools/bazel2gn/bazel_rules:rustc_library.bzl", _rustc_library = "rustc_library")

rustc_binary = _rustc_binary
rustc_library = _rustc_library

# sdk_host_tool does nothing in Bazel right now. It exists to facilitate target
# syncing between GN and Bazel.
def sdk_host_tool(**kwargs):
    pass

# install_host_tools does nothing in Bazel right now. It exists to facilitate
# target syncing between GN and Bazel.
def install_host_tools(**kwargs):
    pass
