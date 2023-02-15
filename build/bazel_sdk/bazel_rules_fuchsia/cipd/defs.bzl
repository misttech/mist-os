# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""CIPD related definitions."""

load(
    "//cipd/private:cipd_tool.bzl",
    _cipd_tool_ext = "cipd_tool_ext",
    _cipd_tool_repository = "cipd_tool_repository",
)
load(
    "//cipd/private:cipd_repository.bzl",
    _cipd_repository = "cipd_repository",
    _fetch_cipd_contents = "fetch_cipd_contents",
    _fetch_cipd_contents_from_https = "fetch_cipd_contents_from_https",
)

cipd_repository = _cipd_repository
cipd_tool_ext = _cipd_tool_ext
cipd_tool_repository = _cipd_tool_repository
fetch_cipd_contents = _fetch_cipd_contents
fetch_cipd_contents_from_https = _fetch_cipd_contents_from_https
