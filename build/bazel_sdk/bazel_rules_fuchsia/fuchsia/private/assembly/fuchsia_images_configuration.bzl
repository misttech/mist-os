# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for declaring an images configuration for a Fuchsia product."""

load(
    ":providers.bzl",
    "FuchsiaAssemblyConfigInfo",
    "FuchsiaFVMNandInfo",
    "FuchsiaFVMSparseInfo",
    "FuchsiaFVMStandardInfo",
    "FuchsiaFsBlobFsInfo",
    "FuchsiaFsEmptyDataInfo",
    "FuchsiaFsReservedInfo",
    "FuchsiaFxfsInfo",
    "FuchsiaProductImagesConfigInfo",
    "FuchsiaVbmetaInfo",
    "FuchsiaZbiInfo",
)

def _collect_file_systems(raw_filesystems, volume_dict):
    for filesystem in raw_filesystems:
        if FuchsiaFsBlobFsInfo in filesystem:
            raw_fs = filesystem[FuchsiaFsBlobFsInfo]
            volume_dict["blob"] = {
                "blob_layout": raw_fs.blobfs_info["layout"],
            }
        elif FuchsiaFsEmptyDataInfo in filesystem:
            volume_dict["data"] = {}
        elif FuchsiaFsReservedInfo in filesystem:
            raw_fs = filesystem[FuchsiaFsReservedInfo]
            volume_dict["reserved"] = {
                "reserved_bytes": raw_fs.reserved_info["slices"],
            }

def _fuchsia_images_configuration_impl(ctx):
    if ctx.attr.images_config:
        return [
            DefaultInfo(
                files = depset(
                    direct = [ctx.file.images_config] + ctx.files.images_config_extra_files,
                ),
            ),
            FuchsiaAssemblyConfigInfo(
                config = ctx.file.images_config,
            ),
        ]

    product_config_file = ctx.actions.declare_file(ctx.label.name + "_product.json")
    volume = {}
    product_images_config = {}
    using_fvm = False
    using_fxfs = False
    for image in ctx.attr.images:
        if FuchsiaZbiInfo in image:
            raw_image = image[FuchsiaZbiInfo]
            product_images_config["image_name"] = raw_image.zbi_name
        elif FuchsiaFVMStandardInfo in image:
            raw_image = image[FuchsiaFVMStandardInfo]
            _collect_file_systems(raw_image.filesystems, volume)
            using_fvm = True
        elif FuchsiaFVMSparseInfo in image:
            raw_image = image[FuchsiaFVMSparseInfo]
            _collect_file_systems(raw_image.filesystems, volume)
            using_fvm = True
        elif FuchsiaFVMNandInfo in image:
            raw_image = image[FuchsiaFVMNandInfo]
            _collect_file_systems(raw_image.filesystems, volume)
            using_fvm = True
        elif FuchsiaFxfsInfo in image:
            using_fxfs = True

    if using_fvm:
        product_images_config["volume"] = {"fvm": volume}
    elif using_fxfs:
        product_images_config["volume"] = "fxfs"
    else:
        product_images_config["volume"] = "none"

    ctx.actions.write(product_config_file, json.encode(product_images_config))
    return [
        DefaultInfo(files = depset(direct = [product_config_file])),
        FuchsiaProductImagesConfigInfo(config = product_config_file),
    ]

fuchsia_images_configuration = rule(
    doc = "Declares an images configuration JSON file for use with ffx assembly.",
    implementation = _fuchsia_images_configuration_impl,
    provides = [FuchsiaProductImagesConfigInfo],
    attrs = {
        # TODO(jayzhuang): Remove this when we decide how to implement imagess configuration so it supports in-tree builds.
        "images_config": attr.label(
            doc = "Path to the generated images configuration file to use",
            allow_single_file = [".json"],
        ),
        "images_config_extra_files": attr.label(
            doc = "A list of files used to provide deps of this images configuration, only used when `images_config` is specified. Note the caller is responsible for ensuring the paths to these files are valid in `images_config`.",
            allow_files = True,
        ),
        "fvm_slice_size": attr.string(
            doc = "size of a slice within the FVM",
        ),
        "images": attr.label_list(
            doc = "Images to include in images configuration",
            providers = [
                [FuchsiaFVMNandInfo],
                [FuchsiaFVMStandardInfo],
                [FuchsiaFVMSparseInfo],
                [FuchsiaFxfsInfo],
                [FuchsiaZbiInfo],
                [FuchsiaVbmetaInfo],
            ],
        ),
    },
)
