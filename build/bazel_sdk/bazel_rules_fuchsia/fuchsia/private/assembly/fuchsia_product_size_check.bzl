# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for running size checker on given image."""

load("@bazel_skylib//lib:paths.bzl", "paths")
load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_args", "get_ffx_assembly_inputs")
load(":providers.bzl", "FuchsiaProductImageInfo", "FuchsiaSizeCheckerInfo")
load(":utils.bzl", "LOCAL_ONLY_ACTION_KWARGS")
load("//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")

# Command for running ffx assembly size-check product.
_SIZE_CHECKER_RUNNER_SH = """
set -e

mkdir -p $FFX_ISOLATE_DIR
{ffx_assembly_args} \
    "--isolate-dir" \
    $FFX_ISOLATE_DIR \
    assembly \
    size-check \
    product \
    --assembly-manifest $IMAGES_PATH \
    --size-breakdown-output $SIZE_FILE \
    --visualization-dir $VISUALIZATION_DIR \
    --gerrit-output $SIZE_REPORT_PRODUCT_FILE \
    --blobfs-creep-budget $CREEP_LIMIT \
    --platform-resources-budget $PLATFORM_RESOURCES_BUDGET
"""

def _fuchsia_product_size_check_impl(ctx):
    images_out = ctx.attr.product_image[FuchsiaProductImageInfo].images_out
    fuchsia_toolchain = get_fuchsia_sdk_toolchain(ctx)

    size_file = ctx.actions.declare_file(ctx.label.name + "_size_breakdown.txt")
    size_report_product_file = ctx.actions.declare_file(ctx.label.name + "_size_report_product.json")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")

    visualization_outputs = [
        ctx.actions.declare_file(paths.join(ctx.label.name + "_visualization", p))
        for p in (
            "data.js",
            "index.html",
            "D3BlobTreeMap.js",
            paths.join("d3_v3", "d3.js"),
            paths.join("d3_v3", "LICENSE"),
        )
    ]
    visualization_dir_path = visualization_outputs[0].dirname

    final_outputs = [size_file, size_report_product_file] + visualization_outputs

    ctx.actions.run_shell(
        inputs = ctx.files.product_image + get_ffx_assembly_inputs(fuchsia_toolchain),
        outputs = final_outputs + [ffx_isolate_dir],
        command = _SIZE_CHECKER_RUNNER_SH.format(
            ffx_assembly_args = " ".join(get_ffx_assembly_args(fuchsia_toolchain)),
        ),
        env = {
            "FFX_ISOLATE_DIR": ffx_isolate_dir.path,
            "IMAGES_PATH": images_out.path + "/images.json",
            "SIZE_FILE": size_file.path,
            "VISUALIZATION_DIR": visualization_dir_path,
            "SIZE_REPORT_PRODUCT_FILE": size_report_product_file.path,
            "CREEP_LIMIT": str(ctx.attr.blobfs_creep_limit),
            "PLATFORM_RESOURCES_BUDGET": str(ctx.attr.platform_resources_budget),
        },
        progress_message = "Size checking for %s" % ctx.label.name,
        **LOCAL_ONLY_ACTION_KWARGS
    )

    return [
        DefaultInfo(files = depset(direct = final_outputs)),
        FuchsiaSizeCheckerInfo(
            size_report = size_report_product_file,
        ),
    ]

fuchsia_product_size_check = rule(
    doc = """Create a size summary of an image.""",
    implementation = _fuchsia_product_size_check_impl,
    provides = [FuchsiaSizeCheckerInfo],
    toolchains = FUCHSIA_TOOLCHAIN_DEFINITION,
    attrs = {
        "product_image": attr.label(
            doc = "fuchsia_product target to check the size of.",
            providers = [FuchsiaProductImageInfo],
            mandatory = True,
        ),
        "blobfs_creep_limit": attr.int(
            doc = "Creep limit for Blobfs, which defines how much blobfs can increase without warning.",
        ),
        "platform_resources_budget": attr.int(
            doc = """Space allocated for shared platform resources.
            These are typically shared libraries provided by the Fuchsia SDK
            that can be included in many different components. It can be helpful
            to isolate these resources into a separate budget to enforce a
            specific number of unique copies, and to have a distinct creep
            budget.""",
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)
