# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@fuchsia_sdk//fuchsia:private_defs.bzl", "FuchsiaProductConfigInfo")
load("//test_utils:json_validator.bzl", "CREATE_VALIDATION_SCRIPT_ATTRS", "create_validation_script_provider")

def _fuchsia_product_configuration_test_impl(ctx):
    product_config_files = ctx.attr.product_config[DefaultInfo].files.to_list()
    product_config_file = (
        ([file for file in product_config_files if file.path.endswith("_product_config.json")] + [None])[0]
    )

    # if it's a directory
    relative_path = None
    if product_config_file == None:
        product_config_file = product_config_files[0]
        relative_path = ctx.attr.product_config[FuchsiaProductConfigInfo].config.removeprefix(product_config_file.path)

    golden_file = ctx.file.golden_file
    return [create_validation_script_provider(ctx, product_config_file, golden_file, relative_path = relative_path)]

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
    } | CREATE_VALIDATION_SCRIPT_ATTRS,
)

def _fuchsia_product_ota_config_test_impl(ctx):
    golden_file = ctx.file.golden_file

    file_to_test = None
    for generated_file in ctx.files.product_config:
        if generated_file.basename == golden_file.basename:
            if file_to_test:
                fail("Found multiple files with the name: %s" % golden_file.basename)
            file_to_test = generated_file
    if not file_to_test:
        fail("Unable to location a file named: %s" % golden_file.basename)

    return [create_validation_script_provider(
        ctx,
        file_to_test,
        golden_file,
        runfiles = ctx.runfiles(files = ctx.files.product_config),
    )]

fuchsia_product_ota_config_test = rule(
    doc = """Validate a generated ota config file from a product config label""",
    test = True,
    implementation = _fuchsia_product_ota_config_test_impl,
    attrs = {
        "product_config": attr.label(
            doc = "Built Product Config.",
            providers = [FuchsiaProductConfigInfo],
            mandatory = True,
        ),
        "golden_file": attr.label(
            doc = "Validate that the file with the same name is produced by the product config rule, and matches in contents.",
            allow_single_file = True,
            mandatory = True,
        ),
    } | CREATE_VALIDATION_SCRIPT_ATTRS,
)
