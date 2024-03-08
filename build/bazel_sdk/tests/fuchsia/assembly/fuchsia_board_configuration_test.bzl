# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@fuchsia_sdk//fuchsia/private/assembly:providers.bzl", "FuchsiaBoardConfigInfo")
load("//test_utils:json_validator.bzl", "CREATE_VALIDATION_SCRIPT_ATTRS", "create_validation_script_provider")

def _fuchsia_board_configuration_test_impl(ctx):
    board_config_file = ctx.attr.board_config[FuchsiaBoardConfigInfo].board_config
    golden_file = ctx.file.golden_file
    return [create_validation_script_provider(ctx, board_config_file, golden_file)]

fuchsia_board_configuration_test = rule(
    doc = """Validate the generated board configuration file.""",
    test = True,
    implementation = _fuchsia_board_configuration_test_impl,
    attrs = {
        "board_config": attr.label(
            doc = "Built Board Config.",
            providers = [FuchsiaBoardConfigInfo],
            mandatory = True,
        ),
        "golden_file": attr.label(
            doc = "Golden file to match against",
            allow_single_file = True,
            mandatory = True,
        ),
    } | CREATE_VALIDATION_SCRIPT_ATTRS,
)
