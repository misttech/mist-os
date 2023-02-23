# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A cc_library generated from a bind library."""

load(":providers.bzl", "FuchsiaBindLibraryInfo")

def _codegen_impl(context):
    sdk = context.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    bindc = sdk.bindc
    base_path = context.attr.name
    name = context.attr.library[FuchsiaBindLibraryInfo].name.replace(".", "/").replace("_bindlib", "")
    bind_source = context.attr.library[DefaultInfo].files.to_list()[0]

    # This declaration is needed in order to get access to the full path.
    root = context.actions.declare_directory(base_path)

    # The generated header fie.
    header_relative = "/bind/" + name + "/cpp" + "/bind.h"
    headers = [context.actions.declare_file(base_path + header_relative)]

    outputs = [root] + headers
    context.actions.run(
        executable = bindc,
        arguments = [
            "generate-cpp",
            "--lint",
            "--output",
            root.path + header_relative,
            bind_source.path,
        ],
        inputs = [
            bind_source,
        ],
        outputs = outputs,
        mnemonic = "BindcGenCc",
    )

    return [
        DefaultInfo(files = depset(headers)),
    ]

# Runs bindc to produce the header file with the constants for the bind_library.
_codegen = rule(
    implementation = _codegen_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    # Files must be generated in genfiles in order for the header to be included
    # anywhere.
    output_to_genfiles = True,
    attrs = {
        "library": attr.label(
            doc = "The bind library to generate code for",
            mandatory = True,
            allow_files = False,
            providers = [FuchsiaBindLibraryInfo],
        ),
    },
)

def fuchsia_bind_cc_library(name, library, deps = [], tags = [], **kwargs):
    """Generates a cc_library() for the given fuchsia_bind_library().

    Args:
      name: Target name. Required.
      library: fuchsia_bind_library() target to generate the code for. Required.
      deps: Additional dependencies.
      tags: Optional tags.
      **kwargs: Remaining args.
    """
    gen_name = "%s_codegen" % name
    _codegen(
        name = gen_name,
        library = library,
    )

    native.cc_library(
        name = name,
        hdrs = [
            ":%s" % gen_name,
        ],
        # This is necessary in order to locate generated headers.
        strip_include_prefix = gen_name,
        deps = deps,
        tags = tags,
        **kwargs
    )
