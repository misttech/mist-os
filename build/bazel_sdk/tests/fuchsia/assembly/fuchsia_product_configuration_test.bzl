# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@fuchsia_sdk//fuchsia/private/assembly:providers.bzl", "FuchsiaProductConfigInfo")
load(":test_utils.bzl", "create_validation_script")

def _fuchsia_product_configuration_test_impl(ctx):
    product_config_file = ctx.attr.product_config[FuchsiaProductConfigInfo].product_config
    golden_file = ctx.file.golden_file

    runfiles = ctx.runfiles(
        files = [
            golden_file,
            product_config_file,
        ],
    ).merge(ctx.attr._json_comparator[DefaultInfo].default_runfiles)
    return [
        DefaultInfo(
            executable = create_validation_script(ctx, product_config_file, golden_file),
            runfiles = runfiles,
            files = depset(
                direct = ctx.files.product_config,
            ),
        ),
    ]

fuchsia_product_configuration_test = rule(
    doc = """Validate the generated product configuration file.""",
    test = True,
    implementation = _fuchsia_product_configuration_test_impl,
    attrs = {
        "product_config": attr.label(
            doc = "Built Product Config.",
            providers = [FuchsiaProductConfigInfo],
            mandatory = True,
        ),
        "golden_file": attr.label(
            doc = "Golden file to match against",
            allow_single_file = True,
            mandatory = True,
        ),
        "_json_comparator": attr.label(
            default = "@fuchsia_sdk//fuchsia/tools:json_comparator",
            executable = True,
            cfg = "exec",
        ),
    },
)
