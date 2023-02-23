# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Rule for defining a pavable Fuchsia image."""

load(
    ":providers.bzl",
    "FuchsiaAssemblyConfigInfo",
    "FuchsiaBoardConfigInfo",
    "FuchsiaProductAssemblyBundleInfo",
    "FuchsiaProductConfigInfo",
    "FuchsiaProductImageInfo",
)
load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_inputs")

# Base source for running ffx assembly product
_PRODUCT_ASSEMBLY_RUNNER_SH_TEMPLATE = """
set -e
ORIG_DIR=$(pwd)
cd $ARTIFACTS_BASE_PATH
mkdir -p $FFX_ISOLATE_DIR
$ORIG_DIR/$FFX \
    --config "assembly_enabled=true,sdk.root=$ORIG_DIR/$SDK_ROOT" \
    --isolate-dir $FFX_ISOLATE_DIR \
    assembly \
    product \
    --product $ORIG_DIR/$PRODUCT_CONFIG_PATH \
    {board_config_arg} \
    --legacy-bundle $ORIG_DIR/$LEGACY_AIB \
    --input-bundles-dir $ORIG_DIR/$PLATFORM_AIB_DIR \
    --outdir $ORIG_DIR/$OUTDIR
$ORIG_DIR/$FFX \
    --config "assembly_enabled=true,sdk.root=$ORIG_DIR/$SDK_ROOT" \
    --isolate-dir $FFX_ISOLATE_DIR \
    assembly \
    create-system \
    --image-assembly-config $ORIG_DIR/$OUTDIR/image_assembly.json \
    --images $ORIG_DIR/$IMAGES_CONFIG_PATH \
    --outdir $ORIG_DIR/$OUTDIR
"""

def _fuchsia_product_image_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    ffx_tool = fuchsia_toolchain.ffx
    legacy_aib = ctx.attr.legacy_aib[FuchsiaProductAssemblyBundleInfo]
    platform_aibs = ctx.attr.platform_aibs[FuchsiaProductAssemblyBundleInfo]
    out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")

    # Product Assembly and create-system
    images_config_info = ctx.attr.image[FuchsiaAssemblyConfigInfo]
    images_config_file = images_config_info.config
    product_config_file = ctx.attr.product_config[FuchsiaProductConfigInfo].product_config
    board_config_file = ctx.attr.board_config[FuchsiaBoardConfigInfo].board_config if ctx.attr.board_config else None
    shell_src = _PRODUCT_ASSEMBLY_RUNNER_SH_TEMPLATE.format(
        board_config_arg = "--board-info $ORIG_DIR/$BOARD_CONFIG_PATH" if board_config_file else "",
    )
    ffx_inputs = get_ffx_assembly_inputs(fuchsia_toolchain)
    ffx_inputs += ctx.files.product_config
    if board_config_file:
        ffx_inputs.append(board_config_file)
    ffx_inputs += legacy_aib.files
    ffx_inputs += platform_aibs.files
    ffx_inputs += ctx.files.product_config
    ffx_inputs += ctx.files.image
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")
    shell_env = {
        "FFX": ffx_tool.path,
        "SDK_ROOT": ctx.attr._sdk_manifest.label.workspace_root,
        "FFX_ISOLATE_DIR": ffx_isolate_dir.path,
        "OUTDIR": out_dir.path,
        "PRODUCT_CONFIG_PATH": product_config_file.path,
        "LEGACY_AIB": legacy_aib.dir.path,
        "PLATFORM_AIB_DIR": platform_aibs.dir.path,
        "IMAGES_CONFIG_PATH": images_config_file.path,
        "ARTIFACTS_BASE_PATH": ctx.attr.artifacts_base_path,
    }
    if board_config_file:
        shell_env["BOARD_CONFIG_PATH"] = board_config_file.path
    ctx.actions.run_shell(
        inputs = ffx_inputs,
        outputs = [
            out_dir,
            # Isolate dirs contain useful debug files like logs, so include it
            # in outputs.
            ffx_isolate_dir,
        ],
        command = shell_src,
        env = shell_env,
        progress_message = "Product Assembly and create-system for %s" % ctx.label.name,
    )
    return [
        DefaultInfo(files = depset(direct = [out_dir, ffx_isolate_dir] + ffx_inputs)),
        OutputGroupInfo(
            debug_files = depset([ffx_isolate_dir]),
            all_files = depset([out_dir, ffx_isolate_dir] + ffx_inputs),
        ),
        FuchsiaProductImageInfo(
            images_out = out_dir,
        ),
    ]

fuchsia_product_image = rule(
    doc = """Declares a Fuchsia product image.""",
    implementation = _fuchsia_product_image_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    provides = [FuchsiaProductImageInfo],
    attrs = {
        "product_config": attr.label(
            doc = "A product configuration target.",
            providers = [FuchsiaProductConfigInfo],
            mandatory = True,
        ),
        "board_config": attr.label(
            doc = "A board configuration target.",
            providers = [FuchsiaBoardConfigInfo],
            # TODO(fxb/119590): Make mandatory once soft transition completes.
            mandatory = False,
        ),
        "image": attr.label(
            doc = "A fuchsia_images_configuration target.",
            providers = [FuchsiaAssemblyConfigInfo],
            mandatory = True,
        ),
        "legacy_aib": attr.label(
            doc = "Legacy AIB for this product.",
            providers = [FuchsiaProductAssemblyBundleInfo],
            mandatory = True,
        ),
        "platform_aibs": attr.label(
            doc = "Platform AIBs for this product.",
            providers = [FuchsiaProductAssemblyBundleInfo],
            mandatory = True,
        ),
        "artifacts_base_path": attr.string(
            doc = "Artifacts base directories that items in config files are relative to.",
            default = ".",
        ),
        "_sdk_manifest": attr.label(
            allow_single_file = True,
            default = "@fuchsia_sdk//:meta/manifest.json",
        ),
    },
)
