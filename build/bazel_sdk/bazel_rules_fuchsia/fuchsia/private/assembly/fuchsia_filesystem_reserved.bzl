# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":providers.bzl", "FuchsiaFsReservedInfo")

def _fuchsia_filesystem_reserved_impl(ctx):
    reserved_info = {
        "type": "reserved",
        "name": ctx.attr.reserved_name,
    }
    if ctx.attr.slices:
        reserved_info["slices"] = int(ctx.attr.slices)
    return [
        FuchsiaFsReservedInfo(
            reserved_name = ctx.attr.reserved_name,
            reserved_info = reserved_info,
        ),
    ]

fuchsia_filesystem_reserved = rule(
    doc = """Generates a reserved filesystem.""",
    implementation = _fuchsia_filesystem_reserved_impl,
    provides = [FuchsiaFsReservedInfo],
    attrs = {
        "reserved_name": attr.string(
            doc = "Name of filesystem",
            default = "internal",
        ),
        "slices": attr.string(
            doc = "The number of slices to reserve.",
        ),
    },
)
