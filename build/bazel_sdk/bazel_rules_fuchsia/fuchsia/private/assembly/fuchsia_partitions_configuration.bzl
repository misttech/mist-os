# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for creating a partitions configuration."""

load(
    ":providers.bzl",
    "FuchsiaPartitionInfo",
    "FuchsiaPartitionsConfigInfo",
)
load(":utils.bzl", "LOCAL_ONLY_ACTION_KWARGS", "select_root_dir_with_file")
load("//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")

def _fuchsia_partitions_configuration(ctx):
    sdk = get_fuchsia_sdk_toolchain(ctx)

    partitions_config = {
        "hardware_revision": ctx.attr.hardware_revision,
        "bootstrap_partitions": [p[FuchsiaPartitionInfo].partition for p in ctx.attr.bootstrap_partitions],
        "bootloader_partitions": [p[FuchsiaPartitionInfo].partition for p in ctx.attr.bootloader_partitions],
        "partitions": [partition[FuchsiaPartitionInfo].partition for partition in ctx.attr.partitions],
        "unlock_credentials": [f.path for f in ctx.files.unlock_credentials],
    }
    content = json.encode_indent(partitions_config, indent = "  ")
    partitions_config_file = ctx.actions.declare_file(ctx.label.name + "_partitions_config.json")
    ctx.actions.write(partitions_config_file, content)

    # Create Partitions Config
    partitions_dir = ctx.actions.declare_directory(ctx.label.name)
    args = [
        "generate",
        "partitions",
        "--config",
        partitions_config_file.path,
        "--output",
        partitions_dir.path,
    ]
    ctx.actions.run(
        executable = sdk.assembly_generate_config,
        arguments = args,
        inputs = ctx.files.bootstrap_partitions + ctx.files.bootloader_partitions + ctx.files.unlock_credentials + [partitions_config_file],
        outputs = [partitions_dir],
        progress_message = "Creating partitions config for %s" % ctx.label.name,
        mnemonic = "CreatePartitionsConfig",
        **LOCAL_ONLY_ACTION_KWARGS
    )

    return [
        DefaultInfo(files = depset([partitions_dir])),
        FuchsiaPartitionsConfigInfo(
            directory = partitions_dir.path,
        ),
    ]

fuchsia_partitions_configuration = rule(
    doc = """Creates a partitions configuration.""",
    implementation = _fuchsia_partitions_configuration,
    toolchains = [FUCHSIA_TOOLCHAIN_DEFINITION],
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
    directory = select_root_dir_with_file(ctx.files.files, "partitions_config.json")
    return [
        DefaultInfo(files = depset(ctx.files.files)),
        FuchsiaPartitionsConfigInfo(
            directory = directory,
        ),
    ]

fuchsia_prebuilt_partitions_configuration = rule(
    doc = """Instantiates a prebuilt partitions configuration.""",
    implementation = _fuchsia_prebuilt_partitions_configuration_impl,
    attrs = {
        "files": attr.label(
            doc = "A filegroup target capturing all prebuilt partition config artifacts.",
            allow_files = True,
            mandatory = True,
        ),
        "partitions_config": attr.label(
            doc = """Deprecated. This no longer does anything.""",
            allow_single_file = [".json"],
        ),
    },
)
