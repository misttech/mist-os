# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for assembling a Fuchsia product."""

load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_inputs")
load(
    ":providers.bzl",
    "FuchsiaBoardConfigDirectoryInfo",
    "FuchsiaBoardConfigInfo",
    "FuchsiaProductAssemblyBundleInfo",
    "FuchsiaProductAssemblyInfo",
    "FuchsiaProductConfigInfo",
    "FuchsiaProductImageInfo",
)

PACKAGE_MODE = struct(
    DISK = "disk",
    EMBED_IN_ZBI = "embed-in-zbi",
    BOOTFS = "bootfs",
)

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
    --board-info $BOARD_CONFIG_PATH \
    --legacy-bundle $LEGACY_AIB \
    --input-bundles-dir $PLATFORM_AIB_DIR \
    {mode_arg} \
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
    {mode_arg} \
    --outdir $OUTDIR
"""

def _fuchsia_product_assembly_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    ffx_tool = fuchsia_toolchain.ffx_assembly
    legacy_bundle = ctx.attr.legacy_bundle[FuchsiaProductAssemblyBundleInfo]
    platform_artifacts = ctx.attr.platform_artifacts[FuchsiaProductAssemblyBundleInfo]
    out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")
    platform_aibs_file = ctx.actions.declare_file(ctx.label.name + "_platform_assembly_input_bundles.json")

    # Create platform_assembly_input_bundles.json file
    ctx.actions.run(
        outputs = [platform_aibs_file],
        inputs = platform_artifacts.files,
        executable = ctx.executable._create_platform_aibs_file,
        arguments = [
            "--platform-aibs",
            platform_artifacts.root,
            "--output",
            platform_aibs_file.path,
        ],
    )

    # Calculate the path to the board configuration file, if it's not directly
    # provided.
    board_config_file_path = None
    board_config_input = None

    if FuchsiaBoardConfigInfo in ctx.attr.board_config:
        board_config = ctx.attr.board_config[FuchsiaBoardConfigInfo]

        # Add all files from the `board_config` attribute as inputs
        board_config_input = ctx.files.board_config

        # The path to the json file itself will be in the provider's board_config
        # field, this needs to be in the arguments to assembly.
        board_config_file_path = board_config.board_config.path

    elif FuchsiaBoardConfigDirectoryInfo in ctx.attr.board_config:
        board_config = ctx.attr.board_config[FuchsiaBoardConfigDirectoryInfo]

        # Add all files from the directory specified in the provider as inputs
        board_config_input = board_config.config_directory

        # Locate the file that is the board_configuration.json, and pass the
        # path to that file as an argument to assembly.
        for file in board_config.config_directory:
            if not board_config_file_path and file.path.endswith("board_configuration.json"):
                board_config_file_path = file.path

        if not board_config_file_path:
            fail("Unable to locate 'board_configuration.json' in BoardConfigDirectoryInfo")

    # Invoke Product Assembly
    product_config_file = ctx.attr.product_config[FuchsiaProductConfigInfo].product_config
    build_type = ctx.attr.product_config[FuchsiaProductConfigInfo].build_type

    shell_src = _PRODUCT_ASSEMBLY_RUNNER_SH_TEMPLATE.format(
        mode_arg = "--mode " + ctx.attr.package_mode if ctx.attr.package_mode else "",
    )

    ffx_inputs = get_ffx_assembly_inputs(fuchsia_toolchain)
    ffx_inputs += ctx.files.product_config
    ffx_inputs += board_config_input
    ffx_inputs += legacy_bundle.files
    ffx_inputs += platform_artifacts.files
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")

    shell_env = {
        "FFX": ffx_tool.path,
        "SDK_ROOT": ctx.attr._sdk_manifest.label.workspace_root,
        "FFX_ISOLATE_DIR": ffx_isolate_dir.path,
        "OUTDIR": out_dir.path,
        "PRODUCT_CONFIG_PATH": product_config_file.path,
        "BOARD_CONFIG_PATH": board_config_file_path,
        "LEGACY_AIB": legacy_bundle.root,
        "PLATFORM_AIB_DIR": platform_artifacts.root,
    }

    for (key, value) in shell_env.items():
        if not value:
            fail("{} was not set".format(key))

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
            build_type = build_type,
        ),
    ]

fuchsia_product_assembly = rule(
    # TODO(http://b/326152150): Make this rule private.
    doc = """Declares a target to product a fully-configured list of artifacts that make up a product.""",
    implementation = _fuchsia_product_assembly_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    provides = [FuchsiaProductAssemblyInfo],
    attrs = {
        "product_config": attr.label(
            doc = "Product configuration used to assemble this product.",
            providers = [FuchsiaProductConfigInfo],
            mandatory = True,
        ),
        "board_config": attr.label(
            doc = "Board configuration used to assemble this product.",
            providers = [[FuchsiaBoardConfigInfo], [FuchsiaBoardConfigDirectoryInfo]],
            mandatory = True,
        ),
        "package_mode": attr.string(
            doc = "Mode indicating where to place packages.",
            values = [PACKAGE_MODE.DISK, PACKAGE_MODE.EMBED_IN_ZBI, PACKAGE_MODE.BOOTFS],
        ),
        "legacy_bundle": attr.label(
            doc = "Legacy AIB for this product.",
            providers = [FuchsiaProductAssemblyBundleInfo],
            mandatory = True,
        ),
        "platform_artifacts": attr.label(
            doc = "Platform artifacts to use for this product.",
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
    product_assembly_out = ctx.attr.product_assembly[FuchsiaProductAssemblyInfo].product_assembly_out
    build_type = ctx.attr.product_assembly[FuchsiaProductAssemblyInfo].build_type

    ffx_inputs = get_ffx_assembly_inputs(fuchsia_toolchain)
    ffx_inputs += ctx.files.product_assembly
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")

    shell_src = _CREATE_SYSTEM_RUNNER_SH_TEMPLATE.format(
        mode_arg = "--mode " + ctx.attr.package_mode if ctx.attr.package_mode else "",
    )

    shell_env = {
        "FFX": ffx_tool.path,
        "SDK_ROOT": ctx.attr._sdk_manifest.label.workspace_root,
        "FFX_ISOLATE_DIR": ffx_isolate_dir.path,
        "OUTDIR": out_dir.path,
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
            build_type = build_type,
        ),
    ]

_fuchsia_product_create_system = rule(
    doc = """Declares a target to generate the images for a Fuchsia product.""",
    implementation = _fuchsia_product_create_system_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    provides = [FuchsiaProductImageInfo],
    attrs = {
        "product_assembly": attr.label(
            doc = "A fuchsia_product_assembly target.",
            providers = [FuchsiaProductAssemblyInfo],
            mandatory = True,
        ),
        "package_mode": attr.string(
            doc = "Mode indicating where to place packages.",
        ),
        "_sdk_manifest": attr.label(
            allow_single_file = True,
            default = "@fuchsia_sdk//:meta/manifest.json",
        ),
    },
)

def fuchsia_product(
        name,
        board_config,
        product_config,
        package_mode = None,
        platform_artifacts = None,
        legacy_bundle = None,
        **kwargs):
    fuchsia_product_assembly(
        name = name + "_product_assembly",
        board_config = board_config,
        product_config = product_config,
        platform_artifacts = platform_artifacts,
        legacy_bundle = legacy_bundle,
        package_mode = package_mode,
    )

    _fuchsia_product_create_system(
        name = name,
        product_assembly = ":" + name + "_product_assembly",
        package_mode = package_mode,
        **kwargs
    )
