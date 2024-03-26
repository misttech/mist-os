# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Public definitions for the foo library"""

load(
    "//tools/bazel-docgen/testdata/foo:foo_internal.bzl",
    _empty_repo = "empty_repo",
    _emtpy = "empty",
)

## Verify that we can grab targets that are imported into the file
empty = _emtpy
empty_repo = _empty_repo

def some_function(name):
    """ a starlark function """
    pass

FooInfo = provider(
    doc = "Some provider",
    fields = {
        "contents": "Some contents",
    },
)
