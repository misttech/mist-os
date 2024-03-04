# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules for wrapping prebuilt platform artifacts."""

load(":providers.bzl", "FuchsiaProductAssemblyBundleInfo")

def _fuchsia_platform_artifacts_impl(ctx):
    return [FuchsiaProductAssemblyBundleInfo(
        root = ctx.file.directory.path,
        files = ctx.files.files,
    )]

fuchsia_platform_artifacts = rule(
    doc = """Wraps a directory of prebuilt platform artifacts.""",
    implementation = _fuchsia_platform_artifacts_impl,
    provides = [FuchsiaProductAssemblyBundleInfo],
    attrs = {
        "directory": attr.label(
            doc = "The directory of prebuilt platform artifacts.",
            mandatory = True,
            allow_single_file = True,
        ),
        "files": attr.label(
            doc = "A filegroup including all files of this prebuilt AIB.",
            mandatory = True,
            allow_files = True,
        ),
    },
)

def _fuchsia_legacy_bundle_impl(ctx):
    if ctx.attr.directory:
        directory = ctx.file.directory.path
    else:
        directory = ctx.file.config.dirname
    return [FuchsiaProductAssemblyBundleInfo(
        root = directory,
        files = ctx.files.files,
    )]

fuchsia_legacy_bundle = rule(
    doc = """Declares a target to wrap a prebuilt Assembly Input Bundle (AIB).""",
    implementation = _fuchsia_legacy_bundle_impl,
    provides = [FuchsiaProductAssemblyBundleInfo],
    attrs = {
        # Deprecated. Use directory.
        "config": attr.label(
            doc = "The assembly_config.json file located at the root of this prebuilt AIB directory.",
            allow_single_file = True,
        ),
        "directory": attr.label(
            doc = "The directory of the prebuilt legacy bundle.",
            allow_single_file = True,
        ),
        "files": attr.label(
            doc = "A filegroup including all files of this prebuilt AIB.",
            mandatory = True,
            allow_files = True,
        ),
    },
)
