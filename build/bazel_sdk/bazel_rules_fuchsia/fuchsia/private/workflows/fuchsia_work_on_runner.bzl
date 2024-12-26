# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Tool for setting up the work_on runner."""

load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load("//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")
load(
    ":utils.bzl",
    "collect_runfiles",
    "wrap_executable",
)

def _fuchsia_work_on_runner_impl(ctx):
    sdk = get_fuchsia_sdk_toolchain(ctx)
    invocation, runner_runfiles = wrap_executable(
        ctx,
        ctx.attr._runner_tool,
        "--ffx",
        sdk.ffx,
    )

    return [
        DefaultInfo(
            executable = invocation,
            runfiles = collect_runfiles(
                ctx,
                runner_runfiles,
                ctx.attr._runner_tool,
                sdk.ffx,
            ),
        ),
    ]

fuchsia_work_on_runner = rule(
    implementation = _fuchsia_work_on_runner_impl,
    toolchains = FUCHSIA_TOOLCHAIN_DEFINITION,
    executable = True,
    attrs = {
        "_runner_tool": attr.label(
            executable = True,
            cfg = "exec",
            default = "//fuchsia/tools:work_on_runner",
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)
