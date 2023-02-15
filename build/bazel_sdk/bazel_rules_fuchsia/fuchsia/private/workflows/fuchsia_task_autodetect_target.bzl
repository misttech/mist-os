# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("//fuchsia/private/workflows:fuchsia_shell_task.bzl", "shell_task_rule")
load(
    "//fuchsia/private:providers.bzl",
    "FuchsiaLocalPackageRepositoryInfo",
    "FuchsiaProductBundleInfo",
)

def _fuchsia_task_autodetect_target_impl(ctx, make_shell_task):
    sdk = ctx.toolchains["@rules_fuchsia//fuchsia:toolchain"]
    command = [
        ctx.attr._detect_tool,
        "--ffx",
        sdk.ffx,
        "--product_bundle",
        ctx.attr.product_bundle[FuchsiaProductBundleInfo].product_name,
        "--product_bundle_repo",
        ctx.attr.product_bundle[FuchsiaProductBundleInfo].repository,
    ]

    if ctx.attr.package_repo:
        command.extend([
            "--package_repo",
            ctx.attr.package_repo[FuchsiaLocalPackageRepositoryInfo].repo_name,
        ])

    return make_shell_task(
        command = command,
    )

# buildifier: disable=unused-variable
(
    _fuchsia_task_autodetect_target,
    _fuchsia_task_autodetect_target_for_test,
    fuchsia_task_autodetect_target,
) = shell_task_rule(
    implementation = _fuchsia_task_autodetect_target_impl,
    # doc = """Creates a package server.""",
    toolchains = ["@rules_fuchsia//fuchsia:toolchain"],
    attrs = {
        "product_bundle": attr.label(
            providers = [[FuchsiaProductBundleInfo]],
            mandatory = True,
        ),
        "package_repo": attr.label(
            providers = [[FuchsiaLocalPackageRepositoryInfo]],
            mandatory = False,
        ),
        "_detect_tool": attr.label(
            default = "//fuchsia/tools:detect_target",
            doc = "The tool to detect the tearget.",
            executable = True,
            cfg = "target",
        ),
    },
)
