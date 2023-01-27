# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaFsBlobFsInfo")

# Define Blob FS Layout
BLOBFS_LAYOUT = struct(
    COMPACT = "compact",
    DEPRECATED_PADDED = "deprecated_padded",
)

def _fuchsia_filesystem_blobfs_impl(ctx):
    blobfs_info = {
        "name": ctx.attr.blobfs_name,
        "compress": ctx.attr.compress,
        "layout": ctx.attr.layout,
        "type": "blobfs",
    }
    if ctx.attr.maximum_bytes:
        blobfs_info["maximum_bytes"] = int(ctx.attr.maximum_bytes)
    if ctx.attr.minimum_data_bytes:
        blobfs_info["minimum_data_bytes"] = int(ctx.attr.minimum_data_bytes)
    if ctx.attr.maximum_contents_size:
        blobfs_info["maximum_contents_size"] = int(ctx.attr.maximum_contents_size)
    if ctx.attr.minimum_inodes:
        blobfs_info["minimum_inodes"] = int(ctx.attr.minimum_inodes)

    return [
        FuchsiaFsBlobFsInfo(
            blobfs_name = ctx.attr.blobfs_name,
            blobfs_info = blobfs_info,
        ),
    ]

fuchsia_filesystem_blobfs = rule(
    doc = """Generates a blobfs filesystem.""",
    implementation = _fuchsia_filesystem_blobfs_impl,
    provides = [FuchsiaFsBlobFsInfo],
    attrs = {
        "blobfs_name": attr.string(
            doc = "Name of filesystem",
            default = "blob",
        ),
        "compress": attr.bool(
            doc = "Whether to compress the volume file.",
            default = True,
        ),
        "layout": attr.string(
            default = BLOBFS_LAYOUT.COMPACT,
            values = [BLOBFS_LAYOUT.COMPACT, BLOBFS_LAYOUT.DEPRECATED_PADDED],
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
        "maximum_contents_size": attr.string(
            doc = "Maximum amount of contents for an assembled blobfs.",
        ),
    },
)
