# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@fuchsia_sdk//fuchsia:private_defs.bzl", "FuchsiaPartitionsConfigInfo")
load("//test_utils:json_validator.bzl", "CREATE_VALIDATION_SCRIPT_ATTRS", "create_validation_script_provider")

def _fuchsia_partitions_configuration_test_impl(ctx):
    partitions_config_file = ctx.attr.partitions_config[FuchsiaPartitionsConfigInfo].config
    golden_file = ctx.file.golden_file
    return [create_validation_script_provider(ctx, partitions_config_file, golden_file)]

fuchsia_partitions_configuration_test = rule(
    doc = """Validate the generated partitions configuration file.""",
    test = True,
    implementation = _fuchsia_partitions_configuration_test_impl,
    attrs = {
        "partitions_config": attr.label(
            doc = "Built partitions Config.",
            providers = [FuchsiaPartitionsConfigInfo],
            mandatory = True,
        ),
        "golden_file": attr.label(
            doc = "Golden file to match against",
            allow_single_file = True,
            mandatory = True,
        ),
    } | CREATE_VALIDATION_SCRIPT_ATTRS,
)
