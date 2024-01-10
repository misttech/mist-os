# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A test that verifies that generated driver runner manifest are correct."""

load("@fuchsia_sdk//fuchsia/private:fuchsia_transition.bzl", "fuchsia_transition")

# buildifier: disable=module-docstring
load("//test_utils:json_validator.bzl", "CREATE_VALIDATION_SCRIPT_ATTRS", "create_validation_script_provider")

def _driver_runner_manifest_test_impl(ctx):
    cml_file = ctx.file.cml_file
    golden_file = ctx.file.golden_file
    return [create_validation_script_provider(ctx, cml_file, golden_file)]

driver_runner_manifest_test = rule(
    doc = """Validate the generated driver runner manifest.""",
    test = True,
    implementation = _driver_runner_manifest_test_impl,
    cfg = fuchsia_transition,
    attrs = {
        "cml_file": attr.label(
            doc = "generated cml file.",
            allow_single_file = True,
            mandatory = True,
        ),
        "golden_file": attr.label(
            doc = "Golden file to match against",
            allow_single_file = True,
            mandatory = True,
        ),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    } | CREATE_VALIDATION_SCRIPT_ATTRS,
)
