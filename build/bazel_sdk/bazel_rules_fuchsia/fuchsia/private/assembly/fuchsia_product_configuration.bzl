# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaAssembledPackageInfo",
    "FuchsiaProductConfigInfo",
)
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

    _walk_json(product_config, [_replace_labels_visitor])

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

    driver_details = []
    for dep in ctx.attr.driver_packages:
        driver_details.append(
            {
                "package": dep[FuchsiaPackageInfo].package_manifest.path,
                "components": dep[FuchsiaPackageInfo].drivers,
            },
        )
        pkg_files.append(dep[FuchsiaPackageInfo].package_manifest)
    product["base_drivers"] = driver_details
    product_config["product"] = product

    product_config_file = ctx.actions.declare_file(ctx.label.name + "_product_config.json")
    content = json.encode_indent(product_config, indent = "  ")
    ctx.actions.write(product_config_file, content)

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
        "raw_config_labels": attr.label_keyed_string_dict(
            doc = """Used internally by fuchsia_product_configuration.
Do not use otherwise""",
            allow_files = True,
            default = {},
        ),
    },
)

def _walk_json(json_dict, visit_node_funcs):
    """Walks a json dictionary, applying the functions in `visit_node_funcs` on every node.

    Args:
        json_dict: The dictionary to walk.
        visit_node_funcs: A function that takes 3 arguments: dictionary, key, value.
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
        for visit_node_func in visit_node_funcs:
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
        driver_packages = None):
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
            and product.base_drivers, which must be specified through the
            following args.
        base_packages: Fuchsia packages to be included in base.
        cache_packages: Fuchsia packages to be included in cache.
        driver_packages: Driver packages to include in product.
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

    def _remove_none_values_visitor(dictionary, key, value):
        """Remove keys with a value of None.

        Some optional keys will necessarily be supplied as 'None' value instead
        of being omitted entirely, because Bazel doesn't allow the use of top-
        level 'if' statements, instead, they can only be used within the value
        of an expression:

          "foo": value if foo else None

        However, we want to strip those 'None' values before generating the json
        from the nested dicts.
        """
        if value == None:
            dictionary.pop(key)

    _walk_json(json_config, [_remove_none_values_visitor, _extract_labels_visitor])

    _fuchsia_product_configuration(
        name = name,
        raw_config = json.encode_indent(json_config, indent = "    "),
        raw_config_labels = extracted_raw_config_labels,
        base_packages = base_packages,
        cache_packages = cache_packages,
        driver_packages = driver_packages,
    )
