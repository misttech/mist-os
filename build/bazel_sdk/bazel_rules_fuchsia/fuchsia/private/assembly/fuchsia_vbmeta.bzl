# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules for declaring a Fuchsia VBMeta image."""

load(
    ":providers.bzl",
    "FuchsiaVbmetaExtraDescriptorInfo",
    "FuchsiaVbmetaInfo",
)

def _fuchsia_vbmeta_extra_descriptor(ctx):
    return [
        FuchsiaVbmetaExtraDescriptorInfo(
            name = ctx.attr.descriptor_name,
            size = ctx.attr.partitions_size,
            flags = ctx.attr.flags,
            min_avb_version = ctx.attr.min_avb_version,
        ),
    ]

fuchsia_vbmeta_extra_descriptor = rule(
    doc = """Generates a descriptor to add to a VBMeta image.""",
    implementation = _fuchsia_vbmeta_extra_descriptor,
    provides = [FuchsiaVbmetaExtraDescriptorInfo],
    attrs = {
        "descriptor_name": attr.string(
            doc = "Name of this descriptor",
            mandatory = True,
        ),
        "partitions_size": attr.string(
            doc = "Size of the partitions in bytes",
            mandatory = True,
        ),
        "flags": attr.string(
            doc = "Custom VBMeta flags to add",
            mandatory = True,
        ),
        "min_avb_version": attr.string(
            doc = "Minimum AVB version to add",
            mandatory = True,
        ),
    },
)

def _fuchsia_vbmeta_impl(ctx):
    return [
        FuchsiaVbmetaInfo(
            vbmeta_name = ctx.attr.vbmeta_name,
            key = ctx.file.key,
            key_metadata = ctx.file.key_metadata,
            extra_descriptors = ctx.attr.extra_descriptors,
        ),
    ]

fuchsia_vbmeta = rule(
    doc = """Generates a fuchsia vbmeta image.""",
    implementation = _fuchsia_vbmeta_impl,
    provides = [FuchsiaVbmetaInfo],
    attrs = {
        "vbmeta_name": attr.string(
            doc = "Name of vbmeta image appeared in image configuration",
            default = "fuchsia",
        ),
        "key": attr.label(
            doc = "The key for signing VBMeta",
            allow_single_file = True,
            default = None,
        ),
        "key_metadata": attr.label(
            doc = "Key metadata to add to the VBMeta",
            allow_single_file = True,
            default = None,
        ),
        "extra_descriptors": attr.label_list(
            doc = "Optional descriptors to add to the VBMeta image.",
            providers = [FuchsiaVbmetaExtraDescriptorInfo],
            default = [],
        ),
    },
)
