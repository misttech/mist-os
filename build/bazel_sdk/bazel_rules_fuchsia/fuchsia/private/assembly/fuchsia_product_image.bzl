# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Rule for defining a pavable Fuchsia image."""

load(
    ":providers.bzl",
    "FuchsiaAssemblyConfigInfo",
    "FuchsiaBoardConfigInfo",
    "FuchsiaProductAssemblyBundleInfo",
    "FuchsiaProductAssemblyInfo",
    "FuchsiaProductConfigInfo",
    "FuchsiaProductImageInfo",
)
load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_inputs")

# Base source for running ffx assembly product
_PRODUCT_ASSEMBLY_RUNNER_SH_TEMPLATE = """
set -e
mkdir -p $FFX_ISOLATE_DIR
$FFX \
    --config "assembly_enabled=true,sdk.root=$SDK_ROOT" \
    --isolate-dir $FFX_ISOLATE_DIR \
    assembly \
    product \
    --product $PRODUCT_CONFIG_PATH \
    {board_config_arg} \
    --legacy-bundle $LEGACY_AIB \
    --input-bundles-dir $PLATFORM_AIB_DIR \
    --outdir $OUTDIR
"""

# Base source for running ffx assembly create-system
_CREATE_SYSTEM_RUNNER_SH_TEMPLATE = """
set -e
mkdir -p $FFX_ISOLATE_DIR
$FFX \
    --config "assembly_enabled=true,sdk.root=$SDK_ROOT" \
    --isolate-dir $FFX_ISOLATE_DIR \
    assembly \
    create-system \
    --image-assembly-config $PRODUCT_ASSEMBLY_OUTDIR/image_assembly.json \
    --images $IMAGES_CONFIG_PATH \
    {mode_arg} \
    --outdir $OUTDIR
"""

def _fuchsia_product_assembly_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    ffx_tool = fuchsia_toolchain.ffx
    legacy_aib = ctx.attr.legacy_aib[FuchsiaProductAssemblyBundleInfo]
    platform_aibs = ctx.attr.platform_aibs[FuchsiaProductAssemblyBundleInfo]
    out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")
    platform_aibs_file = ctx.actions.declare_file(ctx.label.name + "_platform_assembly_input_bundles.json")

    # Create platform_assembly_input_bundles.json file
    ctx.actions.run(
        outputs = [platform_aibs_file],
        inputs = platform_aibs.files,
        executable = ctx.executable._create_platform_aibs_file,
        arguments = [
            "--platform-aibs",
            platform_aibs.dir.path,
            "--output",
            platform_aibs_file.path,
        ],
    )

    # Invoke Product Assembly
    product_config_file = ctx.attr.product_config[FuchsiaProductConfigInfo].product_config
    board_config_file = ctx.attr.board_config[FuchsiaBoardConfigInfo].board_config if ctx.attr.board_config else None
    shell_src = _PRODUCT_ASSEMBLY_RUNNER_SH_TEMPLATE.format(
        board_config_arg = "--board-info $BOARD_CONFIG_PATH" if board_config_file else "",
    )

    ffx_inputs = get_ffx_assembly_inputs(fuchsia_toolchain)
    ffx_inputs += ctx.files.product_config
    if board_config_file:
        ffx_inputs.append(board_config_file)
    ffx_inputs += legacy_aib.files
    ffx_inputs += platform_aibs.files
    ffx_inputs += ctx.files.product_config
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")

    shell_env = {
        "FFX": ffx_tool.path,
        "SDK_ROOT": ctx.attr._sdk_manifest.label.workspace_root,
        "FFX_ISOLATE_DIR": ffx_isolate_dir.path,
        "OUTDIR": out_dir.path,
        "PRODUCT_CONFIG_PATH": product_config_file.path,
        "LEGACY_AIB": legacy_aib.dir.path,
        "PLATFORM_AIB_DIR": platform_aibs.dir.path,
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
        progress_message = "Product Assembly for %s" % ctx.label.name,
    )

    cache_package_list = ctx.actions.declare_file(ctx.label.name + "/bazel_cache_package_manifests.list")
    base_package_list = ctx.actions.declare_file(ctx.label.name + "/bazel_base_package_manifests.list")
    ctx.actions.run(
        outputs = [cache_package_list, base_package_list],
        inputs = [out_dir],
        executable = ctx.executable._create_package_manifest_list,
        arguments = [
            "--images-config",
            out_dir.path + "/image_assembly.json",
            "--cache-package-manifest-list",
            cache_package_list.path,
            "--base-package-manifest-list",
            base_package_list.path,
        ],
    )

    deps = [out_dir, ffx_isolate_dir, cache_package_list, base_package_list, platform_aibs_file] + ffx_inputs

    return [
        DefaultInfo(files = depset(direct = deps)),
        OutputGroupInfo(
            debug_files = depset([ffx_isolate_dir]),
            all_files = depset(deps),
        ),
        FuchsiaProductAssemblyInfo(
            product_assembly_out = out_dir,
            platform_aibs = platform_aibs_file,
        ),
    ]

fuchsia_product_assembly = rule(
    doc = """Declares a Fuchsia product assembly.""",
    implementation = _fuchsia_product_assembly_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    provides = [FuchsiaProductAssemblyInfo],
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
        "_sdk_manifest": attr.label(
            allow_single_file = True,
            default = "@fuchsia_sdk//:meta/manifest.json",
        ),
        "_create_package_manifest_list": attr.label(
            default = "//fuchsia/tools:create_package_manifest_list",
            executable = True,
            cfg = "exec",
        ),
        "_create_platform_aibs_file": attr.label(
            default = "//fuchsia/tools:create_platform_aibs_file",
            executable = True,
            cfg = "exec",
        ),
    },
)

def _fuchsia_product_create_system_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    ffx_tool = fuchsia_toolchain.ffx
    out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")

    # Assembly create-system
    images_config_file = ctx.attr.image[FuchsiaAssemblyConfigInfo].config
    product_assembly_out = ctx.attr.product_assembly[FuchsiaProductAssemblyInfo].product_assembly_out

    ffx_inputs = get_ffx_assembly_inputs(fuchsia_toolchain)
    ffx_inputs += ctx.files.image
    ffx_inputs += ctx.files.product_assembly
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")

    shell_src = _CREATE_SYSTEM_RUNNER_SH_TEMPLATE.format(
        mode_arg = "--mode " + ctx.attr.mode if ctx.attr.mode else "",
    )

    shell_env = {
        "FFX": ffx_tool.path,
        "SDK_ROOT": ctx.attr._sdk_manifest.label.workspace_root,
        "FFX_ISOLATE_DIR": ffx_isolate_dir.path,
        "OUTDIR": out_dir.path,
        "IMAGES_CONFIG_PATH": images_config_file.path,
        "PRODUCT_ASSEMBLY_OUTDIR": product_assembly_out.path,
    }

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
        progress_message = "Assembly Create-system for %s" % ctx.label.name,
    )
    return [
        DefaultInfo(files = depset(direct = [out_dir, ffx_isolate_dir] + ffx_inputs)),
        OutputGroupInfo(
            debug_files = depset([ffx_isolate_dir]),
            all_files = depset([out_dir, ffx_isolate_dir] + ffx_inputs),
        ),
        FuchsiaProductImageInfo(
            images_out = out_dir,
            platform_aibs = ctx.attr.product_assembly[FuchsiaProductAssemblyInfo].platform_aibs,
            product_assembly_out = product_assembly_out,
        ),
    ]

fuchsia_product_create_system = rule(
    doc = """Declares a Fuchsia product create system.""",
    implementation = _fuchsia_product_create_system_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    provides = [FuchsiaProductImageInfo],
    attrs = {
        "image": attr.label(
            doc = "A fuchsia_images_configuration target.",
            providers = [FuchsiaAssemblyConfigInfo],
            mandatory = True,
        ),
        "product_assembly": attr.label(
            doc = "A fuchsia_product_assembly target.",
            providers = [FuchsiaProductAssemblyInfo],
            mandatory = True,
        ),
        "mode": attr.string(
            doc = "Mode indicating where to place packages",
        ),
        "_sdk_manifest": attr.label(
            allow_single_file = True,
            default = "@fuchsia_sdk//:meta/manifest.json",
        ),
    },
)

def fuchsia_product_image(
        name,
        product_config,
        legacy_aib,
        platform_aibs,
        image,
        board_config = None,
        create_system_mode = None,
        **kwargs):
    fuchsia_product_assembly(
        name = name + "_product_assembly",
        board_config = board_config,
        product_config = product_config,
        legacy_aib = legacy_aib,
        platform_aibs = platform_aibs,
    )

    fuchsia_product_create_system(
        name = name,
        product_assembly = ":" + name + "_product_assembly",
        image = image,
        mode = create_system_mode,
        **kwargs
    )
