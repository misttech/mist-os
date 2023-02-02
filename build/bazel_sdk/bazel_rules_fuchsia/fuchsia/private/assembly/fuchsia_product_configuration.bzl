# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(
    ":providers.bzl",
    "FuchsiaAssembledPackageInfo",
    "FuchsiaConnectivityConfigInfo",
    "FuchsiaConnectivityWlanConfigInfo",
    "FuchsiaDevelopmentSupportConfigInfo",
    "FuchsiaDiagnosticsConfigInfo",
    "FuchsiaIdentityConfigInfo",
    "FuchsiaInputConfigInfo",
    "FuchsiaProductConfigInfo",
    "FuchsiaStarnixConfigInfo",
    "FuchsiaStorageConfigInfo",
)
load("//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")

# Define build types
BUILD_TYPES = struct(
    ENG = "eng",
    USER = "user",
    USER_DEBUG = "userdebug",
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

def _create_platform_config(ctx):
    platform = {}
    platform["build_type"] = ctx.attr.build_type
    if len(ctx.attr.additional_serial_log_tags) > 0:
        platform["additional_serial_log_tags"] = ctx.attr.additional_serial_log_tags
    if ctx.attr.identity != None:
        platform["identity"] = ctx.attr.identity[FuchsiaIdentityConfigInfo]
    if ctx.attr.input != None:
        platform["input"] = ctx.attr.input[FuchsiaInputConfigInfo]
    if ctx.attr.connectivity != None:
        connectivity_config = ctx.attr.connectivity[FuchsiaConnectivityConfigInfo]
        platform["connectivity"] = {
            "wlan": connectivity_config.wlan[FuchsiaConnectivityWlanConfigInfo],
        }
    if ctx.attr.development_support != None:
        platform["development_support"] = ctx.attr.development_support[FuchsiaDevelopmentSupportConfigInfo]
    if ctx.attr.starnix != None:
        platform["starnix"] = ctx.attr.starnix[FuchsiaStarnixConfigInfo]
    if ctx.attr.storage != None:
        platform["storage"] = ctx.attr.storage[FuchsiaStorageConfigInfo]
    if ctx.attr.diagnostics != None:
        diagnostics_config = ctx.attr.diagnostics[FuchsiaDiagnosticsConfigInfo]
        platform["diagnostics"] = diagnostics_config

    return platform

def _create_product_config_from_pre_existing_config(ctx):
    # We only support adding base package for now.
    deps = []
    deps += ctx.files.product_config_files
    base_pkg_details = []
    for dep in ctx.attr.base_packages:
        base_pkg_details.append(_create_pkg_detail(dep))
        deps += _collect_file_deps(dep)
    product_config_file = ctx.actions.declare_file(ctx.label.name + "_product_config.json")
    artifact_base_path = product_config_file.path
    if ctx.attr.artifact_base_path:
        artifact_base_path = ctx.file.artifact_base_path.path

    ctx.actions.run(
        outputs = [product_config_file],
        inputs = [ctx.file.product_config],
        executable = ctx.executable._add_base_pkgs,
        arguments = [
            "--product-config",
            ctx.file.product_config.path,
            "--base-details",
            str(base_pkg_details),
            "--updated-product-config",
            product_config_file.path,
            "--relative-base",
            artifact_base_path,
        ],
    )
    deps.append(product_config_file)

    return [
        DefaultInfo(
            files = depset(
                direct = deps,
            ),
        ),
        FuchsiaProductConfigInfo(
            product_config = product_config_file,
        ),
    ]

def _fuchsia_product_configuration_impl(ctx):
    if ctx.attr.product_config:
        return _create_product_config_from_pre_existing_config(ctx)

    product_config = {}
    product_config["platform"] = _create_platform_config(ctx)

    product = {}
    if ctx.attr.session_url:
        product["session_url"] = ctx.attr.session_url
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

    driver_details = []
    for dep in ctx.attr.driver_packages:
        driver_details.append(
            {
                "package": dep[FuchsiaPackageInfo].package_manifest.path,
                "components": dep[FuchsiaPackageInfo].drivers,
            },
        )
        pkg_files += [dep[FuchsiaPackageInfo].package_manifest]
    product["drivers"] = driver_details
    product_config["product"] = product

    product_config_file_rebased = ctx.actions.declare_file(ctx.label.name + "_product_config_rebased.json")
    content = json.encode_indent(product_config, indent = "  ")

    if ctx.attr.artifact_base_path:
        artifact_base_path = ctx.file.artifact_base_path.path
        ctx.actions.run(
            outputs = [product_config_file_rebased],
            executable = ctx.executable._rebase_product_config,
            arguments = [
                "--product-config-content",
                content,
                "--product-config-path",
                product_config_file_rebased.path,
                "--relative-base",
                artifact_base_path,
            ],
        )
    else:
        ctx.actions.write(product_config_file_rebased, content)

    product_config_file = ctx.actions.declare_file(ctx.label.name + "_product_config.json")
    ctx.actions.run(
        inputs = [product_config_file_rebased],
        outputs = [product_config_file],
        executable = ctx.executable._add_parameters,
        arguments = [
            "--product-config-path",
            product_config_file_rebased.path,
            "--additional-bool",
            str(ctx.attr.additional_platform_flags_bool),
            "--additional-string",
            str(ctx.attr.additional_platform_flags_string),
            "--additional-int",
            str(ctx.attr.additional_platform_flags_int),
            "--output",
            product_config_file.path,
        ],
    )

    return [
        DefaultInfo(files = depset(direct = [product_config_file] + pkg_files)),
        FuchsiaProductConfigInfo(
            product_config = product_config_file,
        ),
    ]

fuchsia_product_configuration = rule(
    doc = """Generates a product configuration file.""",
    implementation = _fuchsia_product_configuration_impl,
    toolchains = ["@rules_fuchsia//fuchsia:toolchain"],
    attrs = {
        "build_type": attr.string(
            doc = "Build type of this product.",
            values = [BUILD_TYPES.ENG, BUILD_TYPES.USER, BUILD_TYPES.USER_DEBUG],
        ),
        "identity": attr.label(
            doc = "Identity configuration.",
            providers = [FuchsiaIdentityConfigInfo],
            default = None,
        ),
        "input": attr.label(
            doc = "Input Configuration.",
            providers = [FuchsiaInputConfigInfo],
            default = None,
        ),
        "connectivity": attr.label(
            doc = "Connectivity Configuration.",
            providers = [FuchsiaConnectivityConfigInfo],
            default = None,
        ),
        "diagnostics": attr.label(
            doc = "Diagnostics Configuration.",
            providers = [FuchsiaDiagnosticsConfigInfo],
            default = None,
        ),
        "development_support": attr.label(
            doc = "Developement Support Configuration.",
            providers = [FuchsiaDevelopmentSupportConfigInfo],
            default = None,
        ),
        "starnix": attr.label(
            doc = "Starnix Configuration.",
            providers = [FuchsiaStarnixConfigInfo],
            default = None,
        ),
        "storage": attr.label(
            doc = "Storage Configuration.",
            providers = [FuchsiaStorageConfigInfo],
            default = None,
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
        "driver_packages": attr.label_list(
            doc = "Driver packages to include in product.",
            providers = [FuchsiaPackageInfo],
            default = [],
        ),
        "session_url": attr.string(
            doc = "Session url string that will be included in product_config.json.",
        ),
        #TODO(lijiaming) After the product configuration generation is moved OOT
        #, we can remove this workaround.
        "product_config": attr.label(
            doc = "Relative path of built product_config files. If this file is" +
                  "provided we will skip building product config from scratch.",
            allow_single_file = [".json"],
        ),
        "product_config_files": attr.label(
            doc = "a list of files used to provide deps of product configuration.",
            allow_files = True,
        ),
        "artifact_base_path": attr.label(
            doc = "The artifact base directory. The paths in the product" +
                  "configuration will be relative to this directory. If this" +
                  "path is not provided, paths in product configuration will be" +
                  "relative to product configuration itself",
            allow_single_file = True,
            default = None,
        ),
        "additional_serial_log_tags": attr.string_list(
            doc = """A list of logging tags to forward to the serial console.""",
            default = [],
        ),
        "additional_platform_flags_bool": attr.string_dict(
            doc = """This is a dictionary map from json path of platform config
to a bool value. The values are passed in as string formed true/false.""",
        ),
        "additional_platform_flags_string": attr.string_dict(
            doc = """This is a dictionary map from json path of platform config
to a string value. """,
        ),
        "additional_platform_flags_int": attr.string_dict(
            doc = """This is a dictionary map from json path of platform config
to a int value. The values are passed in as an int string""",
        ),
        "_rebase_product_config": attr.label(
            default = "//fuchsia/tools:rebase_product_config",
            executable = True,
            cfg = "exec",
        ),
        "_add_parameters": attr.label(
            default = "//fuchsia/tools:add_parameters",
            executable = True,
            cfg = "exec",
        ),
        "_add_base_pkgs": attr.label(
            default = "//fuchsia/tools:add_base_pkgs",
            executable = True,
            cfg = "exec",
        ),
    },
)
