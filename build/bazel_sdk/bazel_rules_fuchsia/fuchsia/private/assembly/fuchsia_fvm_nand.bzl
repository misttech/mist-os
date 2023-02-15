# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaFVMNandInfo",
    "FuchsiaFsBlobFsInfo",
    "FuchsiaFsEmptyAccountInfo",
    "FuchsiaFsEmptyDataInfo",
    "FuchsiaFsReservedInfo",
)

def _fuchsia_fvm_nand_impl(ctx):
    fvm_info = {
        "type": "nand",
        "name": ctx.attr.fvm_nand_name,
    }
    if ctx.attr.max_disk_size:
        fvm_info["max_disk_size"] = int(ctx.attr.max_disk_size)
    if ctx.attr.compress:
        fvm_info["compress"] = ctx.attr.compress
    if ctx.attr.block_count:
        fvm_info["block_count"] = int(ctx.attr.block_count)
    if ctx.attr.oob_size:
        fvm_info["oob_size"] = int(ctx.attr.oob_size)
    if ctx.attr.page_size:
        fvm_info["page_size"] = int(ctx.attr.page_size)
    if ctx.attr.pages_per_block:
        fvm_info["pages_per_block"] = int(ctx.attr.pages_per_block)
    return [
        FuchsiaFVMNandInfo(
            fvm_nand_name = ctx.attr.fvm_nand_name,
            filesystems = ctx.attr.filesystems,
            fvm_info = fvm_info,
        ),
    ]

fuchsia_fvm_nand = rule(
    doc = """Generates a FVM prepared for a Nand partition.""",
    implementation = _fuchsia_fvm_nand_impl,
    provides = [FuchsiaFVMNandInfo],
    attrs = {
        "fvm_nand_name": attr.string(
            doc = "Name of fvm file",
            mandatory = True,
        ),
        "filesystems": attr.label_list(
            doc = "Filesystems to use",
            providers = [
                [FuchsiaFsBlobFsInfo],
                [FuchsiaFsEmptyDataInfo],
                [FuchsiaFsEmptyAccountInfo],
                [FuchsiaFsReservedInfo],
            ],
            mandatory = True,
        ),
        "max_disk_size": attr.string(
            doc = "The maximum size the FVM can expand to at runtime.",
        ),
        "compress": attr.bool(
            doc = "Whether to compress the fvm file.",
        ),
        "block_count": attr.string(
            doc = "The number of blocks.",
        ),
        "oob_size": attr.string(
            doc = "The out of bound size.",
        ),
        "page_size": attr.string(
            doc = "Page size as perceived by the FTL.",
        ),
        "pages_per_block": attr.string(
            doc = "Number of pages per erase block unit.",
        ),
    },
)
