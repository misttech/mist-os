# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("@bazel_tools//tools/build_defs/repo:local.bzl", "local_repository")

"""Fuchsia IDK repository extensions."""

_in_tree_repository_tags = tag_class(
    attrs = {
        "path": attr.string(
            doc = "Path to the local IDK directory, relative to workspace root",
            mandatory = True,
        ),
    },
)

def _fuchsia_idk_impl(mctx):
    path = None

    for mod in mctx.modules:
        for in_tree_repo in mod.tags.in_tree_repository:
            if path:
                fail("Multiple in-tree IDK paths received, this is not supported, got:\n{}\n{}".format(local_repository.path, path))
            path = in_tree_repo.path

    local_repository(
        # LINT.IfChange
        name = "fuchsia_in_tree_idk",
        path = path,
        # LINT.ThenChange(//build/regenerator.py)
    )

fuchsia_idk = module_extension(
    implementation = _fuchsia_idk_impl,
    tag_classes = {
        "in_tree_repository": _in_tree_repository_tags,
    },
)
