# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaFsMinFsInfo")

def _fuchsia_filesystem_minfs_impl(ctx):
    minfs_info = {
        "type": "minfs",
        "name": ctx.attr.minfs_name,
    }
    if ctx.attr.maximum_bytes:
        minfs_info["maximum_bytes"] = int(ctx.attr.maximum_bytes)
    if ctx.attr.minimum_data_bytes:
        minfs_info["minimum_data_bytes"] = int(ctx.attr.minimum_data_bytes)
    if ctx.attr.minimum_inodes:
        minfs_info["maximum_contents_size"] = int(ctx.attr.minimum_inodes)
    return [
        FuchsiaFsMinFsInfo(
            minfs_name = ctx.attr.minfs_name,
            minfs_info = minfs_info,
        ),
    ]

fuchsia_filesystem_minfs = rule(
    doc = """Generates a minfs filesystem.""",
    implementation = _fuchsia_filesystem_minfs_impl,
    provides = [FuchsiaFsMinFsInfo],
    attrs = {
        "minfs_name": attr.string(
            doc = "Name of filesystem",
            default = "data",
        ),
        "maximum_bytes": attr.string(
            doc = "Reserve |minimum_data_bytes| and |minimum_inodes| in the FVM, and ensure" +
                  "that the final reserved size does not exceed |maximum_bytes|.",
        ),
        "minimum_data_bytes": attr.string(
            doc = "Reserve space for at least this many data bytes.",
        ),
        "minimum_inodes": attr.string(
            doc = "Reserved space for this many inodes.",
        ),
    },
)
