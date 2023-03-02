# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaAssembledPackageInfo",
    "FuchsiaConnectivityConfigInfo",
    "FuchsiaConnectivityWlanConfigInfo",
    "FuchsiaDevelopmentSupportConfigInfo",
    "FuchsiaDiagnosticsConfigInfo",
    "FuchsiaDriverFrameworkConfigInfo",
    "FuchsiaIdentityConfigInfo",
    "FuchsiaInputConfigInfo",
    "FuchsiaProductConfigInfo",
    "FuchsiaStarnixConfigInfo",
    "FuchsiaStorageConfigInfo",
    "FuchsiaVirtualizationConfigInfo",
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

def _create_platform_config(ctx, initial_value):
    platform = initial_value
    if "build_type" not in platform and ctx.attr.build_type:
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
    if ctx.attr.driver_framework != None:
        platform["driver_framework"] = ctx.attr.driver_framework[FuchsiaDriverFrameworkConfigInfo]
    if ctx.attr.starnix != None:
        platform["starnix"] = ctx.attr.starnix[FuchsiaStarnixConfigInfo]
    if ctx.attr.storage != None:
        platform["storage"] = ctx.attr.storage[FuchsiaStorageConfigInfo]
    if ctx.attr.diagnostics != None:
        diagnostics_config = ctx.attr.diagnostics[FuchsiaDiagnosticsConfigInfo]
        platform["diagnostics"] = diagnostics_config
    if ctx.attr.virtualization != None:
        platform["virtualization"] = ctx.attr.virtualization[FuchsiaVirtualizationConfigInfo]

    return platform

def _fuchsia_product_configuration_impl(ctx):
    product_config = json.decode(ctx.attr.raw_config)

    # Replace "Label(...)" strings in the product config with real label paths from raw_config_labels.
    inverted_raw_config_labels = {}
    for label, string in ctx.attr.raw_config_labels.items():
        inverted_raw_config_labels[string] = label

    def _replace_labels_visitor(dictionary, key, value):
        if type(value) == "string" and value in inverted_raw_config_labels:
            label = inverted_raw_config_labels.get(value)
            label_files = label.files.to_list()
            dictionary[key] = label_files[0].path

    _walk_json(product_config, _replace_labels_visitor)

    product_config["platform"] = _create_platform_config(
        ctx,
        product_config.get("platform", {}),
    )

    product = product_config.get("product", {})
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
        pkg_files.append(dep[FuchsiaPackageInfo].package_manifest)
    product["drivers"] = driver_details
    product_config["product"] = product

    product_config_file_rebased = ctx.actions.declare_file(ctx.label.name + "_product_config_rebased.json")
    content = json.encode_indent(product_config, indent = "  ")
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
        DefaultInfo(files = depset(direct = [product_config_file] + pkg_files + ctx.files.raw_config_labels)),
        FuchsiaProductConfigInfo(
            product_config = product_config_file,
        ),
    ]

_fuchsia_product_configuration = rule(
    doc = """Generates a product configuration file.""",
    implementation = _fuchsia_product_configuration_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "raw_config": attr.string(
            doc = "Raw json config. Used as a base template for the config",
            default = "{}",
        ),
        "build_type": attr.string(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Build type of this product.",
            values = [BUILD_TYPES.ENG, BUILD_TYPES.USER, BUILD_TYPES.USER_DEBUG],
        ),
        "identity": attr.label(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Identity configuration.",
            providers = [FuchsiaIdentityConfigInfo],
            default = None,
        ),
        "input": attr.label(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Input Configuration.",
            providers = [FuchsiaInputConfigInfo],
            default = None,
        ),
        "connectivity": attr.label(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Connectivity Configuration.",
            providers = [FuchsiaConnectivityConfigInfo],
            default = None,
        ),
        "diagnostics": attr.label(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Diagnostics Configuration.",
            providers = [FuchsiaDiagnosticsConfigInfo],
            default = None,
        ),
        "development_support": attr.label(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Developement Support Configuration.",
            providers = [FuchsiaDevelopmentSupportConfigInfo],
            default = None,
        ),
        "driver_framework": attr.label(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Driver Framework Configuration.",
            providers = [FuchsiaDriverFrameworkConfigInfo],
            default = None,
        ),
        "starnix": attr.label(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Starnix Configuration.",
            providers = [FuchsiaStarnixConfigInfo],
            default = None,
        ),
        "storage": attr.label(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Storage Configuration.",
            providers = [FuchsiaStorageConfigInfo],
            default = None,
        ),
        "virtualization": attr.label(
            doc = "(Deprecated: Will be removed as part of fxb/122897) Virtualization Configuration.",
            providers = [FuchsiaVirtualizationConfigInfo],
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
            doc = "(Deprecated: Will be removed as part of fxb/122897) Session url string that will be included in product_config.json.",
        ),
        "additional_serial_log_tags": attr.string_list(
            doc = """(Deprecated: Will be removed as part of fxb/122897) A list of logging tags to forward to the serial console.""",
            default = [],
        ),
        "additional_platform_flags_bool": attr.string_dict(
            doc = """(Deprecated: Will be removed as part of fxb/122897) This is a dictionary map from json path of platform config
to a bool value. The values are passed in as string formed true/false.""",
        ),
        "additional_platform_flags_string": attr.string_dict(
            doc = """(Deprecated: Will be removed as part of fxb/122897) This is a dictionary map from json path of platform config
to a string value. """,
        ),
        "additional_platform_flags_int": attr.string_dict(
            doc = """(Deprecated: Will be removed as part of fxb/122897) This is a dictionary map from json path of platform config
to a int value. The values are passed in as an int string""",
        ),
        "raw_config_labels": attr.label_keyed_string_dict(
            doc = """Used internally by fuchsia_product_configuration.
Do not use otherwise""",
            allow_files = True,
            default = {},
        ),
        "_add_parameters": attr.label(
            default = "//fuchsia/tools:add_parameters",
            executable = True,
            cfg = "exec",
        ),
    },
)

def _walk_json(json_dict, visit_node_func):
    """Walks a json dictionary, applying the function `visit_node_func` on every node.

    Args:
        json_dict: The dictionary to walk.
        visit_node_func: A function that takes 3 arguments: dictionary, key, value.
    """
    nodes_to_visit = []

    def _enqueue(dictionary, k, v):
        nodes_to_visit.append(struct(
            dictionary = dictionary,
            key = k,
            value = v,
        ))

    def _enqueue_dictionary_children(dictionary):
        for key, value in dictionary.items():
            _enqueue(dictionary, key, value)

    _enqueue_dictionary_children(json_dict)

    # Bazel doesn't support recursions, but we don't expect
    # a json object with more than 100K nodes, so this iteration
    # suffices.
    max_nodes = 100000
    for _unused in range(0, max_nodes):
        if not len(nodes_to_visit):
            break
        node = nodes_to_visit.pop()
        visit_node_func(dictionary = node.dictionary, key = node.key, value = node.value)
        if type(node.value) == "dict":
            _enqueue_dictionary_children(node.value)

    if nodes_to_visit:
        fail("More than %s nodes in the input json_dict" % max_nodes)

def fuchsia_product_configuration(
        name,
        json_config = None,
        base_packages = None,
        cache_packages = None,
        driver_packages = None,
        **kargs):
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
            and product.drivers, which must be specified through the
            following args.
        base_packages: Fuchsia packages to be included in base.
        cache_packages: Fuchsia packages to be included in cache.
        driver_packages: Driver packages to include in product.
        TODO(fxb/122897): Remove post migration
        **kargs: Additional attributes propagated into
        _fuchsia_product_configuration rule.
    """

    if not json_config:
        json_config = {}
    if type(json_config) != "dict":
        fail("expecting a dictionary")

    extracted_raw_config_labels = {}

    def _extract_labels_visitor(dictionary, key, value):
        if type(value) == "string" and value.startswith("LABEL("):
            if not value.endswith(")"):
                fail("Syntax error: LABEL does not have closing bracket")
            label = value[6:-1]
            extracted_raw_config_labels[label] = value

    _walk_json(json_config, _extract_labels_visitor)

    _fuchsia_product_configuration(
        name = name,
        raw_config = json.encode_indent(json_config, indent = "    "),
        raw_config_labels = extracted_raw_config_labels,
        base_packages = base_packages,
        cache_packages = cache_packages,
        driver_packages = driver_packages,
        **kargs
    )
