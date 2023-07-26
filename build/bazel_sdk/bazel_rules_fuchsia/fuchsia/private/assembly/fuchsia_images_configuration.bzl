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
    "FuchsiaFsEmptyAccountInfo",
    "FuchsiaFsEmptyDataInfo",
    "FuchsiaFsReservedInfo",
    "FuchsiaFxfsInfo",
    "FuchsiaVbmetaExtraDescriptorInfo",
    "FuchsiaVbmetaInfo",
    "FuchsiaZbiInfo",
)

def _collect_file_systems(raw_filesystems, filesystems_dict):
    file_systems = []
    for filesystem in raw_filesystems:
        if FuchsiaFsBlobFsInfo in filesystem:
            raw_fs = filesystem[FuchsiaFsBlobFsInfo]
            filesystems_dict[raw_fs.blobfs_name] = raw_fs.blobfs_info
            file_systems.append(raw_fs.blobfs_name)
        elif FuchsiaFsEmptyAccountInfo in filesystem:
            raw_fs = filesystem[FuchsiaFsEmptyAccountInfo]
            empty_account_fs = {
                "type": "empty-account",
                "name": raw_fs.empty_account_name,
            }
            filesystems_dict[raw_fs.empty_account_name] = empty_account_fs
            file_systems.append(raw_fs.empty_account_name)
        elif FuchsiaFsEmptyDataInfo in filesystem:
            raw_fs = filesystem[FuchsiaFsEmptyDataInfo]
            empty_data = {
                "type": "empty-data",
                "name": raw_fs.empty_data_name,
            }
            filesystems_dict[raw_fs.empty_data_name] = empty_data
            file_systems.append(raw_fs.empty_data_name)
        elif FuchsiaFsReservedInfo in filesystem:
            raw_fs = filesystem[FuchsiaFsReservedInfo]
            filesystems_dict[raw_fs.reserved_name] = raw_fs.reserved_info
            file_systems.append(raw_fs.reserved_name)
    return file_systems

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

    config_file = ctx.actions.declare_file(ctx.label.name + ".json")
    files = []
    images = []
    fvms = []
    filesystems = {}
    for image in ctx.attr.images:
        if FuchsiaZbiInfo in image:
            raw_image = image[FuchsiaZbiInfo]
            zbi_image = {
                "type": "zbi",
                "name": raw_image.zbi_name,
                "compression": raw_image.compression,
            }
            if raw_image.postprocessing_script != None:
                postprocessing_script = {
                    "path": raw_image.postprocessing_script.path,
                }
                if raw_image.postprocessing_args:
                    postprocessing_script["args"] = raw_image.postprocessing_args
                zbi_image["postprocessing_script"] = postprocessing_script
                files.append(raw_image.postprocessing_script)
            images.append(zbi_image)
        elif FuchsiaVbmetaInfo in image:
            raw_image = image[FuchsiaVbmetaInfo]
            vbmeta_image = {
                "type": "vbmeta",
                "name": raw_image.vbmeta_name,
            }
            if raw_image.key != None:
                vbmeta_image["key"] = raw_image.key.path
                vbmeta_image["key_metadata"] = raw_image.key_metadata.path
                files += [raw_image.key, raw_image.key_metadata]
            if raw_image.extra_descriptors:
                vbmeta_image["additional_descriptors"] = []
                for descriptor in raw_image.extra_descriptors:
                    descriptor = descriptor[FuchsiaVbmetaExtraDescriptorInfo]
                    vbmeta_image["additional_descriptors"].append({
                        "name": descriptor.name,
                        "size": int(descriptor.size),
                        "flags": int(descriptor.flags),
                        "min_avb_version": descriptor.min_avb_version,
                    })
            images.append(vbmeta_image)
        elif FuchsiaFVMStandardInfo in image:
            raw_image = image[FuchsiaFVMStandardInfo]
            standard_fvm = dict(raw_image.fvm_info)
            standard_fvm["filesystems"] = _collect_file_systems(raw_image.filesystems, filesystems)
            fvms.append(standard_fvm)
        elif FuchsiaFVMSparseInfo in image:
            raw_image = image[FuchsiaFVMSparseInfo]
            sparse_fvm = dict(raw_image.fvm_info)
            sparse_fvm["filesystems"] = _collect_file_systems(raw_image.filesystems, filesystems)
            fvms.append(sparse_fvm)
        elif FuchsiaFVMNandInfo in image:
            raw_image = image[FuchsiaFVMNandInfo]
            nand_fvm = dict(raw_image.fvm_info)
            nand_fvm["filesystems"] = _collect_file_systems(raw_image.filesystems, filesystems)
            fvms.append(nand_fvm)
        elif FuchsiaFxfsInfo in image:
            fxfs = {
                "type": "fxfs",
            }
            images.append(fxfs)

    if fvms:
        fvm = {
            "type": "fvm",
            "filesystems": filesystems.values(),
            "outputs": fvms,
        }
        if ctx.attr.fvm_slice_size:
            fvm["slice_size"] = int(ctx.attr.fvm_slice_size)
        images.append(fvm)
    image_config = {"images": images}
    ctx.actions.write(config_file, json.encode(image_config))
    return [
        DefaultInfo(files = depset(direct = [config_file] + files)),
        FuchsiaAssemblyConfigInfo(config = config_file),
    ]

fuchsia_images_configuration = rule(
    doc = "Declares an images configuration JSON file for use with ffx assembly.",
    implementation = _fuchsia_images_configuration_impl,
    provides = [FuchsiaAssemblyConfigInfo],
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
