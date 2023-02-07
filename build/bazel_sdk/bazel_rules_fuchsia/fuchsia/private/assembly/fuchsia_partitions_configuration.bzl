# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for creating a partitions configuration."""

load(
    ":providers.bzl",
    "FuchsiaAssemblyConfigInfo",
    "FuchsiaPartitionInfo",
)

def _collect_partitions(partition_targets):
    partitions = []
    for parition in partition_targets:
        partitions.append(parition[FuchsiaPartitionInfo].partition)
    return partitions

def _fuchsia_partitions_configuration(ctx):
    if ctx.file.partition_config:
        return [
            DefaultInfo(files = depset(direct = [ctx.file.partition_config])),
            FuchsiaAssemblyConfigInfo(config = ctx.file.partition_config),
        ]

    partitions_config_file = ctx.actions.declare_file(ctx.label.name + "_partitions.json")

    partitions_config = {
        "hardware_revision": ctx.attr.hardware_revision,
        "bootstrap_partitions": _collect_partitions(ctx.attr.bootstrap_partitions),
        "bootloader_partitions": _collect_partitions(ctx.attr.bootloader_partitions),
        "partitions": _collect_partitions(ctx.attr.partitions),
    }

    unlock_credentials = []
    for credential in ctx.files.unlock_credentials:
        unlock_credentials.append(credential.path)
    partitions_config["unlock_credentials"] = unlock_credentials
    ctx.actions.write(partitions_config_file, json.encode(partitions_config))

    return [
        DefaultInfo(files = depset(direct = [partitions_config_file] + ctx.files.bootstrap_partitions + ctx.files.bootloader_partitions + ctx.files.unlock_credentials)),
        FuchsiaAssemblyConfigInfo(config = partitions_config_file),
    ]

fuchsia_partitions_configuration = rule(
    doc = """Creates a partitions configuration.""",
    implementation = _fuchsia_partitions_configuration,
    attrs = {
        #TODO(lijiaming) After the partition configuration generation is moved OOT
        #, we can remove this workaround.
        "partition_config": attr.label(
            doc = "Relative path of built partition config file. If this file is" +
                  "provided we will skip building it.",
            allow_single_file = [".json"],
        ),
        "bootstrap_partitions": attr.label_list(
            doc = "Partitions that only flashed in \"fuchsia\" configuration",
            providers = [
                [FuchsiaPartitionInfo],
            ],
        ),
        "bootloader_partitions": attr.label_list(
            doc = "List of bootloader partitions",
            providers = [
                [FuchsiaPartitionInfo],
            ],
        ),
        "partitions": attr.label_list(
            doc = "List of non-bootloader partitions",
            providers = [
                [FuchsiaPartitionInfo],
            ],
        ),
        "hardware_revision": attr.string(
            doc = "name of the hardware that needs to assert before flashing images",
        ),
        "unlock_credentials": attr.label_list(
            doc = "Zip files containing the fastboot unlock credentials",
            allow_files = [".zip"],
        ),
    },
)
