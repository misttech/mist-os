# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaPartitionInfo",
)

SLOT = struct(
    A = "A",  # Primary slot
    B = "B",  # Alternate slot
    R = "R",  # Recovery slot
)

PARTITION_TYPE = struct(
    ZBI = "ZBI",
    VBMETA = "VBMeta",
    FVM = "FVM",
    FXFS = "Fxfs",
)

def _fuchsia_partition_impl(ctx):
    partition = {
        "name": ctx.attr.partition_name,
        "type": ctx.attr.type,
    }
    if ctx.attr.slot != "":
        partition["slot"] = ctx.attr.slot

    return [
        FuchsiaPartitionInfo(
            partition = partition,
        ),
    ]

fuchsia_partition = rule(
    doc = """Define a partition mapping from partition to image.""",
    implementation = _fuchsia_partition_impl,
    provides = [FuchsiaPartitionInfo],
    attrs = {
        "partition_name": attr.string(
            doc = "Name of the partition",
            mandatory = True,
        ),
        "slot": attr.string(
            doc = "The slot of the partition",
            values = [SLOT.A, SLOT.B, SLOT.R],
        ),
        "type": attr.string(
            doc = "Type of this partition",
            mandatory = True,
            values = [PARTITION_TYPE.ZBI, PARTITION_TYPE.VBMETA, PARTITION_TYPE.FVM, PARTITION_TYPE.FXFS],
        ),
    },
)
