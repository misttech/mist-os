# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaFxfsInfo",
)

def _fuchsia_fxfs_impl(ctx):
    fxfs_info = {
        "name": ctx.attr.fxfs_name,
    }
    return [
        FuchsiaFxfsInfo(
            fxfs_name = ctx.attr.fxfs_name,
            fxfs_info = fxfs_info,
        ),
    ]

fuchsia_fxfs = rule(
    doc = """Generates an Fxfs image.""",
    implementation = _fuchsia_fxfs_impl,
    provides = [FuchsiaFxfsInfo],
    attrs = {
        "fxfs_name": attr.string(
            doc = "Name of Fxfs file",
            mandatory = True,
        ),
    },
)
