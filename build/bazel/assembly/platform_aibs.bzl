# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""platform_aibs() rule definition."""

load("@fuchsia_sdk//fuchsia/private/assembly:providers.bzl", "FuchsiaProductAssemblyBundleInfo")
load("//:build/bazel/bazel_version_utils.bzl", "actions_symlink_file_or_directory")
load("@bazel_skylib//lib:paths.bzl", "paths")

def _platform_aibs_impl(ctx):
    aibs_dir_name = ctx.label.name

    root = ctx.actions.declare_file(aibs_dir_name + "/ROOT_MARKER_FOR_BAZEL")
    ctx.actions.run_shell(
        outputs = [root],
        command = "touch $1",
        arguments = [root.path],
    )

    all_outputs = []
    for aib in ctx.attr.aibs:
        aib_info = aib[FuchsiaProductAssemblyBundleInfo]

        aib_dir_path = aib_info.root.dirname
        for file in aib_info.files:
            relative_to_root = paths.relativize(
                path = file.path,
                # Go one level up so AIB name (directory name) is included.
                start = paths.dirname(aib_dir_path),
            )
            output = actions_symlink_file_or_directory(ctx, relative_to_root, file, sibling = root)
            all_outputs.append(output)

    return [
        DefaultInfo(files = depset(direct = all_outputs)),
        FuchsiaProductAssemblyBundleInfo(
            root = root,
            files = all_outputs,
        ),
    ]

platform_aibs = rule(
    doc = """Collect platform AIBs from GN and Bazel into one directory.""",
    implementation = _platform_aibs_impl,
    attrs = {
        "aibs": attr.label_list(
            doc = "a list of platform_aib targets",
            providers = [FuchsiaProductAssemblyBundleInfo],
            default = [],
        ),
    },
)
