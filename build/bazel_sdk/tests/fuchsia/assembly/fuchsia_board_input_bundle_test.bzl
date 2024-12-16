# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@fuchsia_sdk//fuchsia:private_defs.bzl", "FuchsiaBoardInputBundleInfo")
load("//test_utils:json_validator.bzl", "CREATE_VALIDATION_SCRIPT_ATTRS", "create_validation_script_provider")

def _fuchsia_board_input_bundle_test_impl(ctx):
    board_input_bundle_dir = ctx.attr.board_input_bundle[FuchsiaBoardInputBundleInfo].config
    golden_file = ctx.file.golden_file
    return [
        create_validation_script_provider(
            ctx,
            board_input_bundle_dir,
            golden_file,
            relative_path = "board_input_bundle.json",
        ),
    ]

fuchsia_board_input_bundle_test = rule(
    doc = """Validate the generated board input bundle.""",
    test = True,
    implementation = _fuchsia_board_input_bundle_test_impl,
    attrs = {
        "board_input_bundle": attr.label(
            doc = "Built board input bundle.",
            providers = [FuchsiaBoardInputBundleInfo],
            mandatory = True,
        ),
        "golden_file": attr.label(
            doc = "Golden file to match against",
            allow_single_file = True,
            mandatory = True,
        ),
    } | CREATE_VALIDATION_SCRIPT_ATTRS,
)
