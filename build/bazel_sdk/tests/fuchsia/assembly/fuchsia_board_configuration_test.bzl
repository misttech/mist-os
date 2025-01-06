# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@fuchsia_sdk//fuchsia:private_defs.bzl", "FuchsiaBoardConfigInfo")
load("//test_utils:json_validator.bzl", "CREATE_VALIDATION_SCRIPT_ATTRS", "create_validation_script_provider")

def _fuchsia_board_configuration_test_impl(ctx):
    board_config_files = ctx.attr.board_config[FuchsiaBoardConfigInfo].files
    board_config_file = (
        ([file for file in board_config_files if file.path.endswith("_board_configuration")] + [None])[0]
    )
    golden_file = ctx.file.golden_file
    return [create_validation_script_provider(ctx, board_config_file, golden_file, relative_path = "board_configuration.json")]

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

def _fuchsia_hybrid_board_configuration_test_impl(ctx):
    board_config_files = ctx.attr.hybrid_board_config[FuchsiaBoardConfigInfo].files
    bib_file = (
        ([file for file in board_config_files if file.path.endswith("board_input_bundle.json")] + [None])[0]
    )
    return [create_validation_script_provider(ctx, bib_file, ctx.file.golden_bib)]

fuchsia_hybrid_board_configuration_test = rule(
    doc = """Validate the hybrid board has replaced BIB. Note this test assumes there is only one BIB to verify.""",
    test = True,
    implementation = _fuchsia_hybrid_board_configuration_test_impl,
    attrs = {
        "hybrid_board_config": attr.label(
            doc = "Built hybrid board config target",
            providers = [FuchsiaBoardConfigInfo],
            mandatory = True,
        ),
        "golden_bib": attr.label(
            doc = "Golden BIB file to match against",
            allow_single_file = True,
            mandatory = True,
        ),
    } | CREATE_VALIDATION_SCRIPT_ATTRS,
)
