# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Private rule used by fuchsia_product_assembly_bundle."""

load(":providers.bzl", "FuchsiaProductAssemblyBundleInfo")

def _assembly_bundle_impl(ctx):
    return [FuchsiaProductAssemblyBundleInfo(
        dir = ctx.file.dir,
        root = ctx.file.config,
        files = ctx.files.files,
    )]

assembly_bundle = rule(
    implementation = _assembly_bundle_impl,
    provides = [FuchsiaProductAssemblyBundleInfo],
    attrs = {
        "dir": attr.label(
            doc = "(deprecated) path to the assembly bundle directory",
            allow_single_file = True,
        ),
        "config": attr.label(
            doc = "assembly_config.json file located at the root of this AIB",
            allow_single_file = True,
        ),
        "files": attr.label(
            doc = "a list of all files to include in the assembly bundle",
            mandatory = True,
            allow_files = True,
        ),
    },
)
