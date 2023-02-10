# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(
    ":providers.bzl",
    "FuchsiaFVMStandardInfo",
    "FuchsiaFsBlobFsInfo",
    "FuchsiaFsEmptyAccountInfo",
    "FuchsiaFsEmptyDataInfo",
    "FuchsiaFsReservedInfo",
)

def _fuchsia_fvm_standard_impl(ctx):
    fvm_info = {
        "type": "standard",
        "name": ctx.attr.fvm_standard_name,
    }
    if ctx.attr.compress:
        fvm_info["compress"] = ctx.attr.compress
    if ctx.attr.resize_image_file_to_fit:
        fvm_info["resize_image_file_to_fit"] = ctx.attr.resize_image_file_to_fit
    if ctx.attr.truncate_to_length:
        fvm_info["truncate_to_length"] = int(ctx.attr.truncate_to_length)
    return [
        FuchsiaFVMStandardInfo(
            fvm_standard_name = ctx.attr.fvm_standard_name,
            filesystems = ctx.attr.filesystems,
            fvm_info = fvm_info,
        ),
    ]

fuchsia_fvm_standard = rule(
    doc = """Generates a fvm with no modification.""",
    implementation = _fuchsia_fvm_standard_impl,
    provides = [FuchsiaFVMStandardInfo],
    attrs = {
        "fvm_standard_name": attr.string(
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
        "compress": attr.bool(
            doc = "Whether to compress the fvm file.",
        ),
        "resize_image_file_to_fit": attr.bool(
            doc = "Shrink the FVM to fit exactly the contents",
        ),
        "truncate_to_length": attr.string(
            doc = "After the optional resize, truncate the file to this length",
        ),
    },
)
