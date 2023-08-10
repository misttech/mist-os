# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Defines rules for use in WORKSPACE files."""

load(
    "//fuchsia/workspace:fuchsia_clang_repository.bzl",
    _fuchsia_clang_ext = "fuchsia_clang_ext",
    _fuchsia_clang_repository = "fuchsia_clang_repository",
)
load(
    "//fuchsia/workspace:fuchsia_sdk_repository.bzl",
    _fuchsia_sdk_ext = "fuchsia_sdk_ext",
    _fuchsia_sdk_repository = "fuchsia_sdk_repository",
)
load(
    "//fuchsia/workspace:rules_fuchsia_deps.bzl",
    _rules_fuchsia_deps = "rules_fuchsia_deps",
)
load(
    "//fuchsia/workspace:python_runtime_repository.bzl",
    _python_runtime_repository = "python_runtime_repository",
)

# See corresponding `.bzl` files in fuchsia/private for documentation.
fuchsia_sdk_repository = _fuchsia_sdk_repository
fuchsia_sdk_ext = _fuchsia_sdk_ext
rules_fuchsia_deps = _rules_fuchsia_deps
python_runtime_repository = _python_runtime_repository

#TODO(b/295358711) Remove these definitions from here once users have move to loading
#them from clang.bzl.
fuchsia_clang_repository = _fuchsia_clang_repository
fuchsia_clang_ext = _fuchsia_clang_ext
