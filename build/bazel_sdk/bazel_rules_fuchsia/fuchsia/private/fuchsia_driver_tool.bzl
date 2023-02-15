# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":providers.bzl", "FuchsiaDriverToolInfo", "FuchsiaPackageResourcesInfo")

def _find_binary_resource(resources, tool_entry_point):
    bin = None
    if tool_entry_point:
        for resource in resources:
            if resource.dest == tool_entry_point:
                bin = resource
                break
    else:
        for resource in resources:
            if resource.dest.startswith("bin"):
                if bin != None:
                    fail("Multiple binaries found. Please specify a single binary.")
                bin = resource

    if bin == None:
        fail("Unable to find a suitable binary in the given resources.")

    return bin

def _fuchsia_driver_tool_impl(ctx):
    if FuchsiaPackageResourcesInfo in ctx.attr.binary:
        resources = ctx.attr.binary[FuchsiaPackageResourcesInfo].resources
    else:
        resources = []

    bin = _find_binary_resource(resources, ctx.attr.tool_entry_point)

    return [
        FuchsiaDriverToolInfo(
            binary = bin,
            resources = resources,
        ),
    ]

fuchsia_driver_tool = rule(
    doc = """Creates a tool which can be used with ffx driver run-tool.

    This rule will create a tool which can be used in the development of a driver.
    The rule takes a binary which is what will be executed when it runs. When the
    tool is added to a package it can be executed via `bazel run my_pkg.my_tool`.
    This will create a package server, publish the package and call `ffx driver run-tool`

    ```
    fuchsia_cc_binary(
        name = "bin",
        srcs = [ "main.cc" ],
    )

    fuchsia_driver_tool(
        name = "my_tool",
        binary = ":bin",
    )

    fuchsia_package(
        name = "pkg",
        tools = [ ":my_tool" ]
    )

    $ bazel run //pkg.my_tool -- --arg1 foo --arg2 bar
    """,
    implementation = _fuchsia_driver_tool_impl,
    attrs = {
        "binary": attr.label(
            doc = "The binary and its resources.",
            mandatory = True,
            providers = [[FuchsiaPackageResourcesInfo]],
        ),
        "tool_entry_point": attr.string(
            doc = """The path to the binaries entry point in the package.

            The path to the entry point will be inferred by the passed in binary
            if this value is not set. By default, the rule will look for a binary
            in bin/ and use that value. If the package containing the tool does
            not put the executable in bin/ or if bin/ contains multiple entries
            then this attribute must be set.
        """,
        ),
    },
)
