# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules for wrapping prebuilt platform artifacts."""

load(":providers.bzl", "FuchsiaProductAssemblyBundleInfo")

def _get_directory(ctx, rule_name):
    for file in ctx.files.files:
        if file.basename == "assembly_config.json":
            return file.dirname

    fail("\n\n" +
         "When calling {}:\n".format(rule_name) +
         "Could not find assembly_config.json from {}\n".format(ctx.attr.files) +
         "Use the 'directory' attribute to specify the proper directory.\n\n")

def _fuchsia_platform_artifacts_impl(ctx):
    if ctx.file.directory:
        directory = ctx.file.directory.path
    else:
        directory = _get_directory(ctx, "fuchsia_platform_artifacts")

    return [FuchsiaProductAssemblyBundleInfo(
        root = directory,
        files = ctx.files.files,
    )]

fuchsia_platform_artifacts = rule(
    doc = """Wraps a directory of prebuilt platform artifacts.""",
    implementation = _fuchsia_platform_artifacts_impl,
    provides = [FuchsiaProductAssemblyBundleInfo],
    attrs = {
        "directory": attr.label(
            doc = "The directory of prebuilt platform artifacts.",
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
    if ctx.file.directory:
        directory = ctx.file.directory.path
    else:
        directory = _get_directory(ctx, "fuchsia_legacy_bundle")

    return [FuchsiaProductAssemblyBundleInfo(
        root = directory,
        files = ctx.files.files,
    )]

fuchsia_legacy_bundle = rule(
    doc = """Declares a target to wrap a prebuilt Assembly Input Bundle (AIB).""",
    implementation = _fuchsia_legacy_bundle_impl,
    provides = [FuchsiaProductAssemblyBundleInfo],
    attrs = {
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
