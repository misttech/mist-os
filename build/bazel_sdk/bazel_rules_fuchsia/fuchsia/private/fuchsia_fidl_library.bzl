# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for declaring a FIDL library"""

load(":providers.bzl", "FuchsiaFidlLibraryInfo")
load(":fuchsia_fidl_cc_library.bzl", "fuchsia_fidl_cc_library", "get_cc_lib_name")

def _gather_dependencies(deps):
    info = []
    libs_added = []
    for dep in deps:
        for lib in dep[FuchsiaFidlLibraryInfo].info:
            name = lib.name
            if name in libs_added:
                continue
            libs_added.append(name)
            info.append(lib)
    return info

def _fidl_impl(context):
    sdk = context.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    ir = context.outputs.ir
    tables = context.outputs.coding_tables
    library_name = context.attr.library

    info = _gather_dependencies(context.attr.deps)
    info.append(struct(
        name = library_name,
        files = context.files.srcs,
    ))

    files_argument = []
    inputs = []
    for lib in info:
        files_argument += ["--files"] + [f.path for f in lib.files]
        inputs.extend(lib.files)

    # The default Fuchsia target API level of this package
    api_level = context.attr.target_api_level or str(sdk.default_fidl_target_api)

    context.actions.run(
        executable = sdk.fidlc,
        arguments = [
            "--json",
            ir.path,
            "--name",
            library_name,
            "--available",
            "fuchsia:%s" % api_level,
            "--tables",
            tables.path,
        ] + files_argument,
        inputs = inputs,
        outputs = [
            ir,
            tables,
        ],
        mnemonic = "Fidlc",
    )

    return [
        # Exposing the coding tables here so that the target can be consumed as a
        # C++ source.
        DefaultInfo(files = depset([tables])),
        # Passing library info for dependent libraries.
        FuchsiaFidlLibraryInfo(info = info, name = library_name, ir = ir),
    ]

_fidl_library = rule(
    implementation = _fidl_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "library": attr.string(
            doc = "The name of the FIDL library",
            mandatory = True,
        ),
        "target_api_level": attr.string(
            doc = "The version of the Fuchsia platform that this FIDL library will be built for",
            mandatory = False,
        ),
        "srcs": attr.label_list(
            doc = "The list of .fidl source files",
            mandatory = True,
            allow_files = True,
            allow_empty = False,
        ),
        "deps": attr.label_list(
            doc = "The list of libraries this library depends on",
            mandatory = False,
            providers = [FuchsiaFidlLibraryInfo],
        ),
        "cc_bindings": attr.string_list(
            doc = "list of FIDL CC binding types that this library will generate",
            mandatory = False,
        ),
    },
    outputs = {
        # The intermediate representation of the library, to be consumed by bindings
        # generators.
        "ir": "%{name}.fidl.json",
        # The C coding tables.
        "coding_tables": "%{name}_tables.c",
    },
)

def fuchsia_fidl_library(name, srcs, library = None, target_api_level = None, sdk_for_default_deps = None, cc_bindings = [], deps = [], **kwargs):
    """
    A FIDL library.

    Args:
        name: Name of the target
        library: Name of the FIDL library, defaults to the library name
        target_api_level: The version of the Fuchsia platform that this FIDL library will be built for.
        srcs: List of source files.
        cc_bindings: list of FIDL CC binding types to generate. Each binding specified will be represented by
            a new target named {name}_{cc_binding} of type fuchsia_fidl_cc_library.
        deps: List of labels for FIDL libraries this library depends on.
        sdk_for_default_deps: Name of the Bazel workspace where default FIDL library dependencies are defined. If empty or not defined, defaults to @fuchsia_sdk.
        **kwargs: Remaining args to be passed to the C++ binding rules
    """

    if not library:
        library = name

    _fidl_library(
        library = library,
        name = name,
        target_api_level = target_api_level,
        srcs = srcs,
        deps = deps,
        cc_bindings = cc_bindings,
    )

    for cc_binding in cc_bindings:
        fuchsia_fidl_cc_library(
            name = get_cc_lib_name(name, cc_binding),
            library = library,
            binding_type = cc_binding,
            deps = deps,
            sdk_for_default_deps = sdk_for_default_deps,
            **kwargs
        )
