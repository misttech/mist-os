# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for running size checker on given image."""

load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_inputs")
load(":providers.bzl", "FuchsiaProductImageInfo", "FuchsiaSizeCheckerInfo")

# Command for running ffx assembly size-check product.
_SIZE_CHECKER_RUNNER_SH = """
set -e

mkdir -p $FFX_ISOLATE_DIR
$FFX \
    --config "assembly_enabled=true" \
    "--isolate-dir" \
    $FFX_ISOLATE_DIR \
    assembly \
    size-check \
    product \
    --assembly-manifest $IMAGES_PATH \
    --size-breakdown-output $SIZE_FILE \
    --visualization-dir $VISUALIZATION_DIR \
    --gerrit-output $SIZE_REPORT_PRODUCT_FILE \
    --blobfs-creep-budget $CREEP_LIMIT
"""

def _fuchsia_product_size_check_impl(ctx):
    images_out = ctx.attr.product_image[FuchsiaProductImageInfo].images_out
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]

    size_file = ctx.actions.declare_file(ctx.label.name + "_size_breakdown.txt")
    size_report_product_file = ctx.actions.declare_file(ctx.label.name + "_size_report_product.json")
    visualization_dir = ctx.actions.declare_file(ctx.label.name + "_visualization")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")

    ctx.actions.run_shell(
        inputs = ctx.files.product_image + get_ffx_assembly_inputs(fuchsia_toolchain),
        outputs = [size_file, visualization_dir, ffx_isolate_dir, size_report_product_file],
        command = _SIZE_CHECKER_RUNNER_SH,
        env = {
            "FFX": fuchsia_toolchain.ffx.path,
            "FFX_ISOLATE_DIR": ffx_isolate_dir.path,
            "IMAGES_PATH": images_out.path + "/images.json",
            "SIZE_FILE": size_file.path,
            "VISUALIZATION_DIR": visualization_dir.path,
            "SIZE_REPORT_PRODUCT_FILE": size_report_product_file.path,
            "CREEP_LIMIT": str(ctx.attr.blobfs_creep_limit),
        },
        progress_message = "Size checking for %s" % ctx.label.name,
    )
    deps = [size_file, visualization_dir, size_report_product_file]

    return [
        DefaultInfo(files = depset(direct = deps)),
        FuchsiaSizeCheckerInfo(
            size_report = size_report_product_file,
        ),
    ]

fuchsia_product_size_check = rule(
    doc = """Create a size summary of an image.""",
    implementation = _fuchsia_product_size_check_impl,
    provides = [FuchsiaSizeCheckerInfo],
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "product_image": attr.label(
            doc = "fuchsia_product_image target to check size",
            providers = [FuchsiaProductImageInfo],
            mandatory = True,
        ),
        "blobfs_creep_limit": attr.int(
            doc = "Creep limit for Blobfs, this is how much BlobFS contents can increase in one CL",
        ),
    },
)
