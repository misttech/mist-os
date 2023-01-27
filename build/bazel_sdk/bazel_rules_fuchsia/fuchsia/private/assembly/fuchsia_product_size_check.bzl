# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for running size checker on given image."""

load(":providers.bzl", "FuchsiaProductImageInfo")

# Command for running ffx assembly size-check product.
_SIZE_CHECKER_RUNNER_SH = """
set -e

$FFX \
    --config "assembly_enabled=true" \
    assembly \
    size-check \
    product \
    --assembly-manifest $IMAGES_PATH \
    --size-breakdown-output $SIZE_FILE
"""

def _fuchsia_product_size_check_impl(ctx):
    images_out = ctx.attr.product_image[FuchsiaProductImageInfo].images_out
    ffx_tool = ctx.toolchains["@rules_fuchsia//fuchsia:toolchain"].ffx

    size_file = ctx.actions.declare_file(ctx.label.name + "_size_summary")
    ctx.actions.run_shell(
        inputs = ctx.files.product_image + [ffx_tool],
        outputs = [size_file],
        command = _SIZE_CHECKER_RUNNER_SH,
        env = {
            "FFX": ffx_tool.path,
            "IMAGES_PATH": images_out.path + "/images.json",
            "SIZE_FILE": size_file.path,
        },
        progress_message = "Size checking for %s" % ctx.label.name,
    )
    deps = [size_file]

    return [DefaultInfo(files = depset(direct = deps))]

fuchsia_product_size_check = rule(
    doc = """Create a size summary of an image.""",
    implementation = _fuchsia_product_size_check_impl,
    toolchains = ["@rules_fuchsia//fuchsia:toolchain"],
    attrs = {
        "product_image": attr.label(
            doc = "fuchsia_product_image target to check size",
            providers = [FuchsiaProductImageInfo],
            mandatory = True,
        ),
    },
)
