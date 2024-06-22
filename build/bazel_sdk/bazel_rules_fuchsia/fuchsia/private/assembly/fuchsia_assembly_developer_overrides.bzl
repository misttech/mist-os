# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules related to assembly developer overrides."""

load(":util.bzl", "extract_labels", "replace_labels_with_files")
load("//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")
load(
    ":providers.bzl",
    "FuchsiaAssembledPackageInfo",
    "FuchsiaAssemblyDeveloperOverridesInfo",
    "FuchsiaAssemblyDeveloperOverridesListInfo",
)

def _fuchsia_assembly_developer_overrides_impl(ctx):
    overrides = json.decode(ctx.attr.raw_json)
    overrides_file = ctx.actions.declare_file(ctx.label.name + "_developer_overrides.json")

    relative_base = None

    replace_labels_with_files(overrides, ctx.attr.input_labels, relative = relative_base)

    inputs = ctx.files.input_labels

    for dep in ctx.attr.input_labels.keys():
        if type(dep) == "Target":
            # Extract extra inputs from packages.
            if FuchsiaPackageInfo in dep:
                inputs += dep[FuchsiaPackageInfo].files
            elif FuchsiaAssembledPackageInfo in dep:
                inputs += dep[FuchsiaAssembledPackageInfo].files

    ctx.actions.write(overrides_file, json.encode(overrides, indent = "  "))
    outputs = [overrides_file]

    return [
        DefaultInfo(files = depset(direct = outputs + inputs)),
        FuchsiaAssemblyDeveloperOverridesInfo(
            manifest = overrides_file,
            inputs = inputs,
        ),
    ]

_fuchsia_assembly_developer_overrides = rule(
    doc = "Records information about a specific set of developer overrides",
    implementation = _fuchsia_assembly_developer_overrides_impl,
    attrs = {
        "raw_json": attr.string(
            doc = "Raw json string for developer overrides.",
            default = "{}",
        ),
        "input_labels": attr.label_keyed_string_dict(
            doc = """Map of labels in the raw json input to LABEL(label) strings. Labels in the raw json config are replaced by file paths
            identified by their corresponding values in this dict.""",
            allow_files = True,
            default = {},
        ),
    },
)

def fuchsia_assembly_developer_overrides(name, developer_overrides_json):
    """Record information about a set of assembly developer overrides.

    Args:
        name: target name.
        developer_overrides_json: A developer overrides json config, as a starlark dictionary.
            Format of this JSON can be found in this Rust definitions:
                //src/lib/assembly/config_schema/src/developer_overrides.rs

            Key values that take file paths or target labels should be
            declared as a string with the label path wrapped via "LABEL("
            prefix and ")" suffix.
    """
    json_config = developer_overrides_json
    if type(json_config) != "dict":
        fail("expecting a dictionary")

    inputs = extract_labels(json_config)

    _fuchsia_assembly_developer_overrides(
        raw_json = json.encode_indent(json_config, indent = "    "),
        input_labels = extract_labels(json_config),
    )

def _fuchsia_assembly_developer_overrides_list_impl(ctx):
    if len(ctx.attr.patterns) != len(ctx.attr.developer_overrides):
        fail("Expecting two equal-sized lists: %s != %s" % (
            len(ctx.attr.patterns),
            len(ctx.attr.developer_overrides),
        ))

    overrides_info = [
        dep[FuchsiaAssemblyDeveloperOverridesInfo]
        for dep in ctx.attr.developer_overrides
    ]

    return FuchsiaAssemblyDeveloperOverridesListInfo(
        maps = zip(ctx.attr.patterns, overrides_info),
    )

_fuchsia_assembly_developer_overrides_list = rule(
    doc = "Record information about a list of mappings for developer overrides. Uses two parallel lists of equal sizes.",
    implementation = _fuchsia_assembly_developer_overrides_list_impl,
    attrs = {
        "patterns": attr.string_list(
            doc = "List of fuchsia_product() label patterns.",
            default = [],
        ),
        "developer_overrides": attr.label_list(
            doc = "List of labels to fuchsia_assembly_developer_overrides() targets.",
            default = [],
            providers = [FuchsiaAssemblyDeveloperOverridesInfo],
        ),
    },
)

def fuchsia_assembly_developer_overrides_list(name, maps = []):
    """Record information about a list of assembly developer overrides and the assembly product labels they apply to.

    Example usage:

        ```
        fuchsia_assembly_developer_overrides_list(
            name = "my_project_overrides",
            maps = [
                {
                    "assembly": "//products/foo:*",
                    "overrides": ":foo_assembly_overrides",
                },
            ]
        )

        fuchsia_assembly_developer_overrides(
            name = "foo_assembly_overrides",
            developer_overrides_json = {
                "developer_only_options": {
                    "all_packages_in_base": True,
                },
                "base_packages": [
                   "LABEL(//packages/extras:bar)",
                ],
            },
        )
        ```

    Args:
       name: target name.
       maps: A list of dictionaries with this schema:

           assembly: A label pattern string that will be used
              to filter which fuchsia_product() targets the "overrides"
              value applies to.

           overrides: A label to a fuchsia_assembly_developer_overrides() target.

    """
    if type(maps) != "list":
        fail("expecting a list")

    patterns = [m["assembly"] for m in maps]
    overrides = [m["overrides"] for m in maps]

    _fuchsia_assembly_developer_overrides_list(
        name = name,
        patterns = patterns,
        developer_overrides = overrides,
    )
