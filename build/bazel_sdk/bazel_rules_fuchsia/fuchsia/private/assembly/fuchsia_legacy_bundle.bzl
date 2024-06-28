# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules for wrapping prebuilt legacy artifacts."""

load(":providers.bzl", "FuchsiaLegacyBundleInfo")
load(":utils.bzl", "select_single_file")

def _fuchsia_legacy_bundle_impl(ctx):
    if ctx.file.directory:
        directory = ctx.file.directory.path
    else:
        directory = select_single_file(
            ctx.files.files,
            "assembly_config.json",
            "Use the 'directory' attribute to manually specify the legacy AIB's directory.",
        ).dirname

    return [FuchsiaLegacyBundleInfo(
        root = directory,
        files = ctx.files.files,
    )]

fuchsia_legacy_bundle = rule(
    doc = "Declares a target to wrap a prebuilt legacy Assembly Input Bundle (AIB).",
    implementation = _fuchsia_legacy_bundle_impl,
    provides = [FuchsiaLegacyBundleInfo],
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
