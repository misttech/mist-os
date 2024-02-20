# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Facilitates soft transition of rules_rust from 0.16.1 to 0.39.0, where
rust_toolchain.os (previously required) was removed."""

# NOTE: Do not add to this file because it'll be removed as soon as
# rules_rust is upgraded to 0.39.0.

# TODO(jayzhuang): Remove this file when rules_rust is updated to
# 0.39.0.

load("@rules_rust//:version.bzl", "VERSION")
load("@rules_rust//rust:toolchain.bzl", "rust_toolchain")

def rust_toolchain_transition(os, **kwargs):
    if VERSION >= "0.38.0":
        rust_toolchain(**kwargs)
    else:
        # os was required in earlier versions of rules_rust.
        rust_toolchain(os = os, **kwargs)
