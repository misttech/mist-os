# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for creating a partitions configuration."""

load(
    ":providers.bzl",
    "FuchsiaAssemblyConfigInfo",
    "FuchsiaPartitionInfo",
)

def _fuchsia_partitions_configuration(ctx):
    partitions_config_file = ctx.actions.declare_file("%s/partitions_config.json" % ctx.label.name)

    output_files = [partitions_config_file]

    def _symlink(category, file):
        symlink = ctx.actions.declare_file("%s/%s/%s" % (ctx.label.name, category, file.basename))
        output_files.append(symlink)
        ctx.actions.symlink(output = symlink, target_file = file)
        return "%s/%s" % (category, symlink.basename)

    def _rebase_image(category, partition_target):
        partition = partition_target[FuchsiaPartitionInfo].partition
        return partition | {"image": "%s/%s" % (category, partition["image"].rpartition("/")[-1])}

    partitions_config = {
        "hardware_revision": ctx.attr.hardware_revision,
        "bootstrap_partitions": [_rebase_image("bootstrap", partition) for partition in ctx.attr.bootstrap_partitions],
        "bootloader_partitions": [_rebase_image("bootloaders", partition) for partition in ctx.attr.bootloader_partitions],
        "partitions": [partition[FuchsiaPartitionInfo].partition for partition in ctx.attr.partitions],
        "unlock_credentials": [_symlink("credentials", cred) for cred in ctx.files.unlock_credentials],
    }

    for file in ctx.files.bootstrap_partitions:
        _symlink("bootstrap", file)
    for file in ctx.files.bootloader_partitions:
        _symlink("bootloaders", file)

    ctx.actions.write(partitions_config_file, json.encode(partitions_config))

    return [
        DefaultInfo(files = depset(direct = output_files)),
        FuchsiaAssemblyConfigInfo(config = partitions_config_file),
    ]

fuchsia_partitions_configuration = rule(
    doc = """Creates a partitions configuration.""",
    implementation = _fuchsia_partitions_configuration,
    attrs = {
        "bootstrap_partitions": attr.label_list(
            doc = "Partitions that are only flashed in the \"fuchsia\" configuration.",
            providers = [FuchsiaPartitionInfo],
        ),
        "bootloader_partitions": attr.label_list(
            doc = "List of bootloader partitions.",
            providers = [FuchsiaPartitionInfo],
        ),
        "partitions": attr.label_list(
            doc = "List of non-bootloader partitions.",
            providers = [FuchsiaPartitionInfo],
        ),
        "hardware_revision": attr.string(
            doc = "Name of the hardware that needs to assert before flashing images.",
        ),
        "unlock_credentials": attr.label_list(
            doc = "List of zip files containing the fastboot unlock credentials.",
            allow_files = [".zip"],
        ),
    },
)

def _fuchsia_prebuilt_partitions_configuration_impl(ctx):
    return [
        DefaultInfo(files = depset(direct = ctx.files.srcs)),
        FuchsiaAssemblyConfigInfo(config = ctx.file.partitions_config),
    ]

fuchsia_prebuilt_partitions_configuration = rule(
    doc = """Instantiates a prebuilt partitions configuration.""",
    implementation = _fuchsia_prebuilt_partitions_configuration_impl,
    attrs = {
        "srcs": attr.label(
            doc = "A filegroup target capturing all prebuilt partition config artifacts.",
            allow_files = True,
            mandatory = True,
        ),
        "partitions_config": attr.label(
            doc = "Relative path of prebuilt partition config file. Must be present within `srcs` as well.",
            allow_single_file = [".json"],
            mandatory = True,
        ),
    },
)
