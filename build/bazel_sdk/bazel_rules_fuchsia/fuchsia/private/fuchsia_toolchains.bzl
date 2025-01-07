# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

""" Helper functions for generating fuchsia sdk toolchains. """

def _fuchsia_toolchain_decl_impl(ctx):
    ctx.file("WORKSPACE.bazel", content = "", executable = False)
    ctx.file(
        "BUILD.bazel",
        content = """
toolchain(
  name = "{toolchain_name}",
  toolchain = "{toolchain_path}",
  toolchain_type = "@rules_fuchsia//fuchsia/toolchains:sdk",
  visibility = ["//visibility:public"],
)
""".format(toolchain_name = ctx.name + "_toolchain", toolchain_path = ctx.attr.toolchain_path),
        executable = False,
    )

_fuchsia_toolchain_decl = repository_rule(
    implementation = _fuchsia_toolchain_decl_impl,
    attrs = {
        "toolchain_path": attr.string(mandatory = True),
    },
)

def get_fuchsia_sdk_toolchain(ctx):
    """A helper function which makes it easier to get the fuchsia toolchain.

    This method makes it so you do not need to rely on hardcoded values for the
    toolchain.

    Args:
        ctx: The rule context.

    Returns:
        The fuchsia sdk tool chain.
    """
    sdk = ctx.toolchains["@rules_fuchsia//fuchsia/toolchains:sdk"]

    if not sdk:
        fail("No fuchsia toolchain registered. Please call register_fuchsia_sdk_toolchain in your WORKSPACE file.")
    return sdk

# These toolchain definitions should be used in conjunction with get_fuchsia_sdk_toolchain
# when working with fuchsia toolchains.
FUCHSIA_TOOLCHAIN_DEFINITION = "@rules_fuchsia//fuchsia/toolchains:sdk"

def register_fuchsia_sdk_toolchain(
        name = "fuchsia_sdk_toolchain_decl",
        toolchain_path = "@fuchsia_sdk//:fuchsia_toolchain_info"):
    """ Registers the fuchsia sdk toolchain.

    This method should be called in your WORKSPACE file to register the fuchsia
    toolchain. It defaults to using the toolchain defined in the fuchsia_sdk
    repository but a different name can be used if needed.

    The toolchain is assumed to exist at the root of the given repo but the path
    can be provided if needed.

    Args:
        name: The toolchain decl repository.
        toolchain_path: The fully qualified path to the toolchain. This should only
          be needed if you are using a non-standard SDK.
    """
    _fuchsia_toolchain_decl(
        name = name,
        toolchain_path = toolchain_path,
    )
    native.register_toolchains("@" + name + "//:all")
