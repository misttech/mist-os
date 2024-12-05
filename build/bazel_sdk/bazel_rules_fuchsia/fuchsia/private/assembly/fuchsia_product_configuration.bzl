# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for declaring a Fuchsia product configuration."""

load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load("//fuchsia/private:fuchsia_package.bzl", "get_driver_component_manifests")
load("//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")
load("//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaAssembledPackageInfo",
    "FuchsiaOmahaOtaConfigInfo",
    "FuchsiaProductConfigInfo",
)
load(
    ":utils.bzl",
    "combine_directories",
    "extract_labels",
    "replace_labels_with_files",
)

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

def _create_pkg_detail(dep, relative = None):
    package_manifest_path = None
    configs = None

    # Find the package manifest and configs from the input depending on the provider.
    if FuchsiaPackageInfo in dep:
        package_manifest_path = dep[FuchsiaPackageInfo].package_manifest.path
    else:
        package_manifest_path = dep[FuchsiaAssembledPackageInfo].package.package_manifest.path
        configs = dep[FuchsiaAssembledPackageInfo].configs

    # Relativize the path if necessary.
    if relative:
        package_manifest_path = package_manifest_path.removeprefix(relative + "/")

    # If we have configs, return them.
    if configs:
        config_data = []
        for config in configs:
            config_data.append(
                {
                    "destination": config.destination,
                    "source": config.source.path,
                },
            )
        return {"manifest": package_manifest_path, "config_data": config_data}
    else:
        return {"manifest": package_manifest_path}

def _collect_file_deps(dep):
    if FuchsiaPackageInfo in dep:
        return dep[FuchsiaPackageInfo].files

    return dep[FuchsiaAssembledPackageInfo].files

def _collect_debug_symbols(dep):
    if FuchsiaPackageInfo in dep:
        return getattr(dep[FuchsiaPackageInfo], "build_id_dirs", [])
    return getattr(dep[FuchsiaAssembledPackageInfo], "build_id_dirs", [])

def _fuchsia_product_configuration_impl(ctx):
    product_config = json.decode(ctx.attr.product_config)
    product_config_file = ctx.actions.declare_file(ctx.label.name + "_product_config.json")

    relative_base = None

    # If relative_paths is set, relativize paths in the product config relative
    # to the directory containing the product config.
    # Otherwise, paths are relative to the execroot.
    if (ctx.attr.relative_paths):
        relative_base = product_config_file.dirname
        product_config["file_relative_paths"] = True

    replace_labels_with_files(product_config, ctx.attr.product_config_labels, relative = relative_base)

    platform = product_config.get("platform", {})
    build_type = platform.get("build_type")
    product = product_config.get("product", {})
    packages = {}

    output_files = []
    build_id_dirs = []
    base_pkg_details = []
    for dep in ctx.attr.base_packages:
        base_pkg_details.append(_create_pkg_detail(dep, relative_base))
        output_files += _collect_file_deps(dep)
        build_id_dirs += _collect_debug_symbols(dep)
    packages["base"] = base_pkg_details

    cache_pkg_details = []
    for dep in ctx.attr.cache_packages:
        cache_pkg_details.append(_create_pkg_detail(dep, relative_base))
        output_files += _collect_file_deps(dep)
        build_id_dirs += _collect_debug_symbols(dep)
    packages["cache"] = cache_pkg_details
    product["packages"] = packages

    base_driver_details = []
    for dep in ctx.attr.base_driver_packages:
        package_detail = _create_pkg_detail(dep, relative_base)
        base_driver_details.append(
            {
                "package": package_detail["manifest"],
                "components": get_driver_component_manifests(dep),
            },
        )
        output_files += _collect_file_deps(dep)
    product["base_drivers"] = base_driver_details

    product_config["product"] = product

    if ctx.attr.ota_configuration:
        swd_config = product_config["platform"].setdefault("software_delivery", {})
        update_checker_config = swd_config.setdefault("update_checker", {})
        omaha_config = update_checker_config.setdefault("omaha_client", {})

        ota_config_info = ctx.attr.ota_configuration[FuchsiaOmahaOtaConfigInfo]

        channels_file = ctx.actions.declare_file("channel_config.json")
        ctx.actions.write(channels_file, ota_config_info.channels)
        output_files.append(channels_file)

        omaha_config["channels_path"] = channels_file.path

        tuf_config_paths = []
        for (hostname, repo_config) in ota_config_info.tuf_repositories.items():
            repo_config_file = ctx.actions.declare_file(hostname + ".json")
            ctx.actions.write(repo_config_file, repo_config)
            tuf_config_paths.append(repo_config_file.path)
            output_files.append(repo_config_file)
        swd_config["tuf_config_paths"] = tuf_config_paths

    content = json.encode_indent(product_config, indent = "  ")
    ctx.actions.write(product_config_file, content)
    output_files.append(product_config_file)

    return [
        DefaultInfo(files = depset(direct = output_files + ctx.files.product_config_labels + ctx.files.deps)),
        FuchsiaProductConfigInfo(
            config = product_config_file.path,
            build_type = build_type,
            build_id_dirs = build_id_dirs,
        ),
    ]

def _fuchsia_prebuilt_product_configuration_impl(ctx):
    return [
        DefaultInfo(files = depset(direct = ctx.files.files)),
        FuchsiaProductConfigInfo(
            config = ctx.file.product_config.path,
            build_type = ctx.attr.build_type,
            build_id_dirs = [],
        ),
    ]

_fuchsia_prebuilt_product_configuration = rule(
    doc = "Use a prebuilt product configuration directory for hybrid assembly.",
    implementation = _fuchsia_prebuilt_product_configuration_impl,
    toolchains = FUCHSIA_TOOLCHAIN_DEFINITION,
    attrs = {
        "product_config": attr.label(
            doc = "The product assembly input artifacts directory containing the product config.",
            allow_single_file = True,
            mandatory = True,
        ),
        "files": attr.label_list(
            doc = "All files referenced by the product config. This should be the entire contents of the product input artifacts directory.",
            mandatory = True,
        ),
        "build_type": attr.string(
            doc = "Build type of the product config. Must match the prebuilts.",
            mandatory = True,
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)

_fuchsia_product_configuration = rule(
    doc = """Generates a product configuration file.""",
    implementation = _fuchsia_product_configuration_impl,
    toolchains = FUCHSIA_TOOLCHAIN_DEFINITION,
    attrs = {
        "product_config": attr.string(
            doc = "Raw json config. Used as a base template for the config.",
            default = "{}",
        ),
        "product_config_labels": attr.label_keyed_string_dict(
            doc = "Map of labels in the raw json config to LABEL(label) strings. Labels in the raw json config are replaced by file paths identified by their corresponding values in this dict.",
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
        "ota_configuration": attr.label(
            doc = "OTA configuration to include in the product. Only for use with products that use Omaha.",
            providers = [FuchsiaOmahaOtaConfigInfo],
        ),
        "relative_paths": attr.bool(
            doc = "Whether to generate an Assembly product configuration with relative path, so it can be relocated.",
            default = False,
        ),
        "deps": attr.label_list(
            doc = "Additional dependencies that must be built before this target is built.",
            default = [],
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)

def fuchsia_prebuilt_product_configuration(
        name,
        product_config_dir,
        product_config_filename,
        build_type,
        **kwargs):
    _all_files_target = "{}_all_files".format(name)
    native.filegroup(
        name = _all_files_target,
        srcs = native.glob(["{}/**/*".format(product_config_dir)]),
    )
    _fuchsia_prebuilt_product_configuration(
        name = name,
        product_config = product_config_dir + "/" + product_config_filename,
        files = [":{}".format(_all_files_target)],
        build_type = build_type,
        **kwargs
    )

def fuchsia_product_configuration(
        name,
        product_config_json = None,
        base_packages = None,
        cache_packages = None,
        base_driver_packages = None,
        ota_configuration = None,
        relative_paths = False,
        **kwargs):
    """A new implementation of fuchsia_product_configuration that takes raw a json config.

    Args:
        name: Name of the rule.
        product_config_json: product assembly json config, as a starlark dictionary.
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

            TODO(https://fxbug.dev/42073826): Point to document instead of Rust definition
        base_packages: Fuchsia packages to be included in base.
        cache_packages: Fuchsia packages to be included in cache.
        base_driver_packages: Base driver packages to include in product.
        ota_configuration: OTA configuration to use with the product.
        relative_paths: Whether to generate an Assembly product configuration
            with relative path, so it can be relocated.
        **kwargs: Common bazel rule args passed through to the implementation rule.
    """

    json_config = product_config_json
    if not product_config_json:
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
        ota_configuration = ota_configuration,
        relative_paths = relative_paths,
        **kwargs
    )

def fuchsia_hybrid_product_configuration_impl(ctx):
    product_dir_name = ctx.label.name
    product_dir = ctx.actions.declare_directory(product_dir_name)
    product_configuration = ctx.attr.product_configuration[FuchsiaProductConfigInfo].config.split("/")[-1]

    product_config_outputs = []
    src_config_dirname = "/".join(ctx.attr.product_configuration[FuchsiaProductConfigInfo].config.split("/")[:-1])

    subdirectories_to_overwrite = {}
    for label, package_dir_name in ctx.attr.replace_packages.items():
        src_dir = "/".join(label[FuchsiaPackageInfo].package_manifest.path.split("/")[:-1])
        subdirectories_to_overwrite[src_dir] = package_dir_name

    inputs = []
    for label in ctx.attr.replace_packages.keys():
        inputs += label[FuchsiaPackageInfo].files

    combine_directories(
        ctx,
        src_config_dirname,
        product_dir.path,
        depset(inputs, transitive = [ctx.attr.product_configuration[DefaultInfo].files]),
        [product_dir],
        subdirectories_to_overwrite = subdirectories_to_overwrite,
    )

    return [
        DefaultInfo(files = depset([product_dir])),
        FuchsiaProductConfigInfo(
            config = product_dir.path + "/" + product_configuration,
            build_type = ctx.attr.product_configuration[FuchsiaProductConfigInfo].build_type,
            build_id_dirs = [],
        ),
    ]

fuchsia_hybrid_product_configuration = rule(
    doc = "Combine in-tree packages with a prebuilt product config from out of tree for hybrid assembly",
    implementation = fuchsia_hybrid_product_configuration_impl,
    provides = [FuchsiaProductConfigInfo],
    attrs = {
        "product_configuration": attr.label(
            doc = "Prebuilt product config",
            providers = [FuchsiaProductConfigInfo],
            mandatory = True,
        ),
        "replace_packages": attr.label_keyed_string_dict(
            doc = "List of directories and packages to replace them with",
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)
