# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")

def _print_debug_info_impl(ctx):
    sdk = ctx.toolchains["//fuchsia/toolchains:sdk"]

    executable_file = ctx.actions.declare_file(ctx.label.name + "_dump.sh")
    content = """#!/bin/bash
    echo "----------------------------------------------------------"
    echo "========================================================="
    echo "*** SDK Version:"
    echo "========================================================="
    sdk_version=$({ffx} sdk version)
    echo "sdk_version: $({ffx} sdk version)"
    (
        # Check to see if the user has an sdk-integration repo so
        # we can grab the git hash. This will not be needed once we
        # release the rules with the core sdk.
        if cd "$BUILD_WORKSPACE_DIRECTORY/third_party/sdk-integration"; then
            git_revision=$(git rev-parse HEAD)
            echo "sdk-integration git rev: $git_revision"
        else
            echo "Cannot find an sdk-integration repository"
        fi
    )
    echo

    echo "========================================================="
    echo "*** Output from uname -v:"
    echo "========================================================="
    uname -v
    echo

    echo "========================================================="
    echo "*** Output from ffx version -v:"
    echo "========================================================="
    "{ffx}" version -v
    echo

    echo "========================================================="
    echo "*** Output from ffx doctor --no-config:"
    echo "========================================================="
    "{ffx}" doctor --no-config
    echo

    echo "========================================================="
    echo "*** Output from clang --version:"
    echo "========================================================="
    if [[ -x "{clang}" ]]; then
        "{clang}" --version
    else
        echo "Cannot find a suitable clang binary: {clang}"
    fi

    echo "----------------------------------------------------------"
    """.format(
        ffx = sdk.ffx.short_path,
        clang = ctx.executable._clang_bin.short_path,
    )

    ctx.actions.write(
        output = executable_file,
        content = content,
        is_executable = True,
    )

    runfiles = ctx.runfiles(files = [sdk.ffx, ctx.executable._clang_bin])
    return [DefaultInfo(executable = executable_file, runfiles = runfiles)]

print_debug_info = rule(
    doc = """ Creates an action which gathers debug information and prints it.""",
    implementation = _print_debug_info_impl,
    toolchains = ["//fuchsia/toolchains:sdk"],
    attrs = {
        "_clang_bin": attr.label(
            default = "@fuchsia_clang//:bin/clang",
            executable = True,
            cfg = "exec",
            allow_single_file = True,
        ),
    } | COMPATIBILITY.HOST_ATTRS,
    executable = True,
)
