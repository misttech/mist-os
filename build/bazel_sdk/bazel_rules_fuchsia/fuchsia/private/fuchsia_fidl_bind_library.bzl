# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A fuchsia_bind_library backed by a FIDL library."""

load("@bazel_skylib//lib:paths.bzl", "paths")
load(":fuchsia_bind_library.bzl", "fuchsia_bind_library")
load(":providers.bzl", "FuchsiaFidlLibraryInfo")

def _bindlibgen_impl(ctx):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    bindc = sdk.bindc

    ir = ctx.attr.library[FuchsiaFidlLibraryInfo].ir
    fidl_lib_name = ctx.attr.library[FuchsiaFidlLibraryInfo].name

    base_path = ctx.attr.name

    # The generated bind library file
    bindlib = ctx.actions.declare_file(paths.join(base_path, "fidl_bindlibs", fidl_lib_name + ".bind"))

    ctx.actions.run(
        executable = bindc,
        arguments = [
            "generate-bind",
            "--output",
            bindlib.path,
            ir.path,
        ],
        inputs = [ir],
        outputs = [bindlib],
        mnemonic = "FidlGenBindlib",
    )

    return [
        DefaultInfo(files = depset([bindlib])),
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
