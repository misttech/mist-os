# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for defining an Assembly Input Bundle (AIB)."""

load(":providers.bzl", "FuchsiaProductAssemblyBundleInfo")

def _assembly_bundle_impl(ctx):
    return [FuchsiaProductAssemblyBundleInfo(
        root = ctx.file.config,
        files = ctx.files.files,
    )]

assembly_bundle = rule(
    doc = """Declares a target to wrap a prebuilt Assembly Input Bundle (AIB).""",
    implementation = _assembly_bundle_impl,
    provides = [FuchsiaProductAssemblyBundleInfo],
    attrs = {
        "config": attr.label(
            doc = "The assembly_config.json file located at the root of this prebuilt AIB directory.",
            allow_single_file = True,
        ),
        "files": attr.label(
            doc = "A filegroup including all files of this prebuilt AIB.",
            mandatory = True,
            allow_files = True,
        ),
    },
)
