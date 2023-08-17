# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaAssembledPackageInfo",
    "FuchsiaProductConfigInfo",
)
load(":util.bzl", "extract_labels", "replace_labels_with_files")
load("//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")

# Define build types
BUILD_TYPES = struct(
    ENG = "eng",
    USER = "user",
    USER_DEBUG = "userdebug",
)

# Define input device types option
INPUT_DEVICE_TYPE = struct(
    BUTTON = "button",
    KEYBOARD = "keyboard",
    MOUSE = "mouse",
    TOUCHSCREEN = "touchscreen",
)

def _create_pkg_detail(dep):
    if FuchsiaPackageInfo in dep:
        return {"manifest": dep[FuchsiaPackageInfo].package_manifest.path}

    package = dep[FuchsiaAssembledPackageInfo].package
    configs = dep[FuchsiaAssembledPackageInfo].configs
    config_data = []
    for config in configs:
        config_data.append(
            {
                "destination": config.destination,
                "source": config.source.path,
            },
        )
    return {"manifest": package.package_manifest.path, "config_data": config_data}

def _collect_file_deps(dep):
    if FuchsiaPackageInfo in dep:
        return dep[FuchsiaPackageInfo].files

    return dep[FuchsiaAssembledPackageInfo].files

def _fuchsia_product_configuration_impl(ctx):
    product_config = json.decode(ctx.attr.product_config)
    replace_labels_with_files(product_config, ctx.attr.product_config_labels)
    product = product_config.get("product", {})
    packages = {}

    pkg_files = []
    base_pkg_details = []
    for dep in ctx.attr.base_packages:
        base_pkg_details.append(_create_pkg_detail(dep))
        pkg_files += _collect_file_deps(dep)
    packages["base"] = base_pkg_details

    cache_pkg_details = []
    for dep in ctx.attr.cache_packages:
        cache_pkg_details.append(_create_pkg_detail(dep))
        pkg_files += _collect_file_deps(dep)
    packages["cache"] = cache_pkg_details
    product["packages"] = packages

    base_driver_details = []
    for dep in ctx.attr.base_driver_packages:
        base_driver_details.append(
            {
                "package": dep[FuchsiaPackageInfo].package_manifest.path,
                "components": dep[FuchsiaPackageInfo].drivers,
            },
        )
        pkg_files += _collect_file_deps(dep)
    product["base_drivers"] = base_driver_details

    product_config["product"] = product

    product_config_file = ctx.actions.declare_file(ctx.label.name + "_product_config.json")
    content = json.encode_indent(product_config, indent = "  ")
    ctx.actions.write(product_config_file, content)

    return [
        DefaultInfo(files = depset(direct = [product_config_file] + pkg_files + ctx.files.product_config_labels)),
        FuchsiaProductConfigInfo(
            product_config = product_config_file,
        ),
    ]

_fuchsia_product_configuration = rule(
    doc = """Generates a product configuration file.""",
    implementation = _fuchsia_product_configuration_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "product_config": attr.string(
            doc = "Raw json config. Used as a base template for the config",
            default = "{}",
        ),
        "product_config_labels": attr.label_keyed_string_dict(
            doc = """Map of labels to LABEL(label) strings in the product config.""",
            allow_files = True,
            default = {},
        ),
        "base_packages": attr.label_list(
            doc = "Fuchsia packages to be included in base.",
            providers = [
                [FuchsiaAssembledPackageInfo],
                [FuchsiaPackageInfo],
            ],
            default = [],
        ),
        "cache_packages": attr.label_list(
            doc = "Fuchsia packages to be included in cache.",
            providers = [
                [FuchsiaAssembledPackageInfo],
                [FuchsiaPackageInfo],
            ],
            default = [],
        ),
        "base_driver_packages": attr.label_list(
            doc = "Base-driver packages to include in product.",
            providers = [FuchsiaPackageInfo],
            default = [],
        ),
    },
)

def fuchsia_product_configuration(
        name,
        json_config = None,
        base_packages = None,
        cache_packages = None,
        base_driver_packages = None,
        **kwarg):
    """A new implementation of fuchsia_product_configuration that takes raw a json config.

    Args:
        name: Name of the rule.
        TODO(fxb/122898): Point to document instead of Rust definition
        json_config: product assembly json config, as a starlark dictionary.
            Format of this JSON config can be found in this Rust definitions:
               //src/lib/assembly/config_schema/src/assembly_config.rs

            Key values that take file paths should be declared as a string with
            the label path wrapped via "LABEL(" prefix and ")" suffix. For
            example:
            ```
            {
                "platform": {
                    "some_file": "LABEL(//path/to/file)",
                },
            },
            ```

            All assembly json inputs are supported, except for product.packages
            and product.base_drivers, which must be
            specified through the following args.
        base_packages: Fuchsia packages to be included in base.
        cache_packages: Fuchsia packages to be included in cache.
        base_driver_packages: Base driver packages to include in product.
        **kwarg: Common bazel rule args passed through to the implementation rule.
    """

    if not json_config:
        json_config = {}
    if type(json_config) != "dict":
        fail("expecting a dictionary")

    _fuchsia_product_configuration(
        name = name,
        product_config = json.encode_indent(json_config, indent = "    "),
        product_config_labels = extract_labels(json_config),
        base_packages = base_packages,
        cache_packages = cache_packages,
        base_driver_packages = base_driver_packages,
        **kwarg,
    )