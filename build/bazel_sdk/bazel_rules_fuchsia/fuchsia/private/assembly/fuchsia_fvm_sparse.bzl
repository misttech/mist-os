# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaFVMSparseInfo",
    "FuchsiaFsBlobFsInfo",
    "FuchsiaFsEmptyAccountInfo",
    "FuchsiaFsEmptyDataInfo",
    "FuchsiaFsReservedInfo",
)

def _fuchsia_fvm_sparse_impl(ctx):
    fvm_info = {
        "type": "sparse",
        "name": ctx.attr.fvm_sparse_name,
    }
    if ctx.attr.max_disk_size:
        fvm_info["max_disk_size"] = int(ctx.attr.max_disk_size)
    return [
        FuchsiaFVMSparseInfo(
            fvm_sparse_name = ctx.attr.fvm_sparse_name,
            filesystems = ctx.attr.filesystems,
            fvm_info = fvm_info,
        ),
    ]

fuchsia_fvm_sparse = rule(
    doc = """Generates a fvm that is compressed sparse.""",
    implementation = _fuchsia_fvm_sparse_impl,
    provides = [FuchsiaFVMSparseInfo],
    attrs = {
        "fvm_sparse_name": attr.string(
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
    },
)
