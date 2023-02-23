# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A fuchsia_bind_library backed by a FIDL library."""

load(":fuchsia_bind_library.bzl", "fuchsia_bind_library")
load(":providers.bzl", "FuchsiaFidlLibraryInfo")

def _bindlibgen_impl(context):
    sdk = context.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    bindc = sdk.bindc

    ir = context.attr.library[FuchsiaFidlLibraryInfo].ir
    fidl_lib_name = context.attr.library[FuchsiaFidlLibraryInfo].name

    base_path = context.attr.name

    # This declaration is needed in order to get access to the full path.
    root = context.actions.declare_directory(base_path)

    # The generated bind library file
    bindlib_relative = "/fidl_bindlibs/" + fidl_lib_name + ".bind"
    bindlib = [context.actions.declare_file(base_path + bindlib_relative)]

    outputs = [root] + bindlib
    context.actions.run(
        executable = bindc,
        arguments = [
            "generate-bind",
            "--output",
            root.path + bindlib_relative,
            ir.path,
        ],
        inputs = [
            ir,
        ],
        outputs = outputs,
        mnemonic = "FidlGenBindlib",
    )

    return [
        DefaultInfo(files = depset(bindlib)),
    ]

# Runs bindc to produce the bind library file.
_bindlibgen = rule(
    implementation = _bindlibgen_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "library": attr.label(
            doc = "The FIDL library to generate bind library for",
            mandatory = True,
            allow_files = False,
            providers = [FuchsiaFidlLibraryInfo],
        ),
    },
)

def fuchsia_fidl_bind_library(name, library, **kwargs):
    """Generates fuchsia_bind_library() for the given fidl_library.

    Args:
      name: Target name. Required.
      library: fidl_library() target to generate the language bindings for. Required.
      **kwargs: Remaining args.
    """
    gen_name = "%s_gen" % name

    _bindlibgen(
        name = gen_name,
        library = library,
    )

    fuchsia_bind_library(
        name = name,
        srcs = [":%s" % gen_name],
        **kwargs
    )
