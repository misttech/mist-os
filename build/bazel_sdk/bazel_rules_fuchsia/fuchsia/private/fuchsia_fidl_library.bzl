# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for declaring a FIDL library"""

load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load(":fuchsia_api_level.bzl", "FUCHSIA_API_LEVEL_ATTRS", "get_fuchsia_api_level")
load(":fuchsia_fidl_cc_library.bzl", "fuchsia_fidl_cc_library", "get_cc_lib_name")
load(":providers.bzl", "FuchsiaFidlLibraryInfo")
load("//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")

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
    sdk = get_fuchsia_sdk_toolchain(context)
    ir = context.outputs.ir
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

    api_level = get_fuchsia_api_level(context)

    context.actions.run(
        executable = sdk.fidlc,
        arguments = [
            "--json",
            ir.path,
            "--name",
            library_name,
            "--available",
            "fuchsia:%s" % api_level,
        ] + files_argument,
        inputs = inputs,
        outputs = [ir],
        mnemonic = "Fidlc",
    )

    return [
        # Passing library info for dependent libraries.
        FuchsiaFidlLibraryInfo(info = info, name = library_name, ir = ir),
    ]

_fidl_library = rule(
    implementation = _fidl_impl,
    toolchains = [FUCHSIA_TOOLCHAIN_DEFINITION],
    attrs = {
        "library": attr.string(
            doc = "The name of the FIDL library",
            mandatory = True,
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
    } | COMPATIBILITY.HOST_ATTRS | FUCHSIA_API_LEVEL_ATTRS,
    outputs = {
        # The intermediate representation of the library, to be consumed by bindings
        # generators.
        "ir": "%{name}.fidl.json",
    },
)

def fuchsia_fidl_library(name, srcs, library = None, sdk_for_default_deps = None, cc_bindings = [], deps = [], **kwargs):
    """
    A FIDL library.

    Args:
        name: Name of the target
        library: Name of the FIDL library, defaults to the library name
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
        srcs = srcs,
        deps = deps,
        cc_bindings = cc_bindings,
        **kwargs
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
