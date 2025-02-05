# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for assembling a Fuchsia product."""

load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_args", "get_ffx_assembly_inputs")
load(
    ":providers.bzl",
    "FuchsiaAssemblyDeveloperOverridesInfo",
    "FuchsiaAssemblyDeveloperOverridesListInfo",
    "FuchsiaBoardConfigInfo",
    "FuchsiaLegacyBundleInfo",
    "FuchsiaPlatformArtifactsInfo",
    "FuchsiaProductAssemblyInfo",
    "FuchsiaProductConfigInfo",
    "FuchsiaProductImageInfo",
)
load(":utils.bzl", "LOCAL_ONLY_ACTION_KWARGS")
load("//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")

# Base source for running ffx assembly create-system
_CREATE_SYSTEM_RUNNER_SH_TEMPLATE = """
set -e
mkdir -p $FFX_ISOLATE_DIR
{ffx_assembly_args} \
    --isolate-dir $FFX_ISOLATE_DIR \
    assembly \
    create-system \
    --image-assembly-config $PRODUCT_ASSEMBLY_OUTDIR/image_assembly.json \
    --outdir $OUTDIR
"""

def _match_assembly_pattern_string(label, pattern):
    package = label.package
    assembly_pattern = pattern.removeprefix("//")
    if assembly_pattern.endswith("/*"):
        # Match any target in the package or below
        return package.startswith(assembly_pattern.removesuffix("/*"))
    elif assembly_pattern.endswith(":*"):
        # Match package exactly.
        return package == assembly_pattern.removesuffix(":*")
    else:
        # Match label exactly.
        return assembly_pattern == "%s:%s" % (package, label.name)

def _fuchsia_product_assembly_impl(ctx):
    fuchsia_toolchain = get_fuchsia_sdk_toolchain(ctx)
    platform_artifacts = ctx.attr.platform_artifacts[FuchsiaPlatformArtifactsInfo]
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
        progress_message = "Gathering AIBs for %s" % ctx.label,
        **LOCAL_ONLY_ACTION_KWARGS
    )

    # Calculate the path to the board configuration file, if it's not directly
    # provided.
    board_config = ctx.attr.board_config[FuchsiaBoardConfigInfo]

    # Add all files from the `board_config` attribute as inputs
    board_config_input = board_config.files

    # The path to the json file itself will be in the provider's board_config
    # field, this needs to be in the arguments to assembly.
    board_config_file_path = board_config.config

    # Invoke Product Assembly
    # TODO(https://fxbug.dev/391674348): Assembly should take the product config as a directory.
    product_config = ctx.attr.product_config[FuchsiaProductConfigInfo]
    product_config_file_path = product_config.directory
    if product_config.config_path:
        product_config_file_path += "/" + product_config.config_path
    else:
        product_config_file_path += "/product_configuration.json"

    build_type = product_config.build_type
    build_id_dirs = []
    build_id_dirs += product_config.build_id_dirs
    build_id_dirs += ctx.attr.board_config[FuchsiaBoardConfigInfo].build_id_dirs

    ffx_inputs = get_ffx_assembly_inputs(fuchsia_toolchain)
    ffx_inputs += ctx.files.product_config
    ffx_inputs += board_config_input
    ffx_inputs += platform_artifacts.files
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")

    ffx_invocation = get_ffx_assembly_args(fuchsia_toolchain) + [
        "--isolate-dir",
        ffx_isolate_dir.path,
        "assembly",
        "product",
        "--product",
        product_config_file_path,
        "--board-info",
        board_config_file_path,
        "--input-bundles-dir",
        platform_artifacts.root,
        "--outdir",
        out_dir.path,
        "--package-validation",
        ctx.attr.package_validation,
    ]

    if ctx.attr.legacy_bundle:
        legacy_bundle = ctx.attr.legacy_bundle[FuchsiaLegacyBundleInfo]
        ffx_invocation.extend(["--legacy-bundle", legacy_bundle.root])
        ffx_inputs += legacy_bundle.files

    # Add developer overrides manifest and inputs if necessary.
    overrides_maps = ctx.attr._developer_overrides_list[FuchsiaAssemblyDeveloperOverridesListInfo].maps
    for (pattern_string, overrides_label) in overrides_maps.items():
        if _match_assembly_pattern_string(ctx.label, pattern_string):
            overrides_info = overrides_label[FuchsiaAssemblyDeveloperOverridesInfo]
            ffx_inputs += overrides_info.inputs
            ffx_invocation.extend([
                "--developer-overrides",
                overrides_info.manifest.path,
            ])

    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    shell_src = [
        "set -e",
        "mkdir -p " + ffx_isolate_dir.path,
        " ".join(_ffx_invocation),
    ]

    ctx.actions.run_shell(
        inputs = ffx_inputs,
        outputs = [
            out_dir,
            # Isolate dirs contain useful debug files like logs, so include it
            # in outputs.
            ffx_isolate_dir,
        ],
        command = "\n".join(shell_src),
        progress_message = "Product Assembly for %s" % ctx.label,
        **LOCAL_ONLY_ACTION_KWARGS
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
        progress_message = "Creating package manifests list for %s" % ctx.label,
        **LOCAL_ONLY_ACTION_KWARGS
    )

    deps = [out_dir, ffx_isolate_dir, cache_package_list, base_package_list, platform_aibs_file] + ffx_inputs

    return [
        DefaultInfo(files = depset(deps)),
        OutputGroupInfo(
            debug_files = depset([ffx_isolate_dir]),
            all_files = depset(deps),
        ),
        FuchsiaProductAssemblyInfo(
            product_assembly_out = out_dir,
            platform_aibs = platform_aibs_file,
            build_type = build_type,
            build_id_dirs = build_id_dirs,
        ),
    ]

_fuchsia_product_assembly = rule(
    doc = """Declares a target to product a fully-configured list of artifacts that make up a product.""",
    implementation = _fuchsia_product_assembly_impl,
    toolchains = [FUCHSIA_TOOLCHAIN_DEFINITION],
    provides = [FuchsiaProductAssemblyInfo],
    attrs = {
        "product_config": attr.label(
            doc = "Product configuration used to assemble this product.",
            providers = [FuchsiaProductConfigInfo],
            mandatory = True,
        ),
        "board_config": attr.label(
            doc = "Board configuration used to assemble this product.",
            providers = [FuchsiaBoardConfigInfo],
            mandatory = True,
        ),
        "legacy_bundle": attr.label(
            doc = "Legacy AIB for this product.",
            providers = [FuchsiaLegacyBundleInfo],
        ),
        "platform_artifacts": attr.label(
            doc = "Platform artifacts to use for this product.",
            providers = [FuchsiaPlatformArtifactsInfo],
            mandatory = True,
        ),
        "package_validation": attr.string(
            doc = """Whether package validation errors should be treated as
            errors or warnings. Ignoring validation errors may lead to a buggy
            or nonfunctional product!""",
            default = "error",
            values = ["error", "warning"],
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
        "_developer_overrides_list": attr.label(
            default = "//fuchsia:assembly_developer_overrides_list",
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)

def _fuchsia_product_create_system_impl(ctx):
    fuchsia_toolchain = get_fuchsia_sdk_toolchain(ctx)
    out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")

    # Assembly create-system
    product_assembly_out = ctx.attr.product_assembly[FuchsiaProductAssemblyInfo].product_assembly_out
    build_type = ctx.attr.product_assembly[FuchsiaProductAssemblyInfo].build_type
    build_id_dirs = ctx.attr.product_assembly[FuchsiaProductAssemblyInfo].build_id_dirs

    ffx_inputs = get_ffx_assembly_inputs(fuchsia_toolchain)
    ffx_inputs += ctx.files.product_assembly
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")

    shell_src = _CREATE_SYSTEM_RUNNER_SH_TEMPLATE.format(
        ffx_assembly_args = " ".join(get_ffx_assembly_args(fuchsia_toolchain)),
    )

    shell_env = {
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
        progress_message = "Assembly Create-system for %s" % ctx.label,
        **LOCAL_ONLY_ACTION_KWARGS
    )
    return [
        DefaultInfo(files = depset([out_dir, ffx_isolate_dir] + ffx_inputs)),
        OutputGroupInfo(
            debug_files = depset([ffx_isolate_dir]),
            all_files = depset([out_dir, ffx_isolate_dir] + ffx_inputs),
        ),
        FuchsiaProductImageInfo(
            images_out = out_dir,
            platform_aibs = ctx.attr.product_assembly[FuchsiaProductAssemblyInfo].platform_aibs,
            product_assembly_out = product_assembly_out,
            build_type = build_type,
            build_id_dirs = build_id_dirs,
        ),
    ]

_fuchsia_product_create_system = rule(
    doc = """Declares a target to generate the images for a Fuchsia product.""",
    implementation = _fuchsia_product_create_system_impl,
    toolchains = [FUCHSIA_TOOLCHAIN_DEFINITION],
    provides = [FuchsiaProductImageInfo],
    attrs = {
        "product_assembly": attr.label(
            doc = "A fuchsia_product_assembly target.",
            providers = [FuchsiaProductAssemblyInfo],
            mandatory = True,
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)

def fuchsia_product(
        name,
        board_config,
        product_config,
        platform_artifacts = None,
        legacy_bundle = None,
        package_validation = None,
        **kwargs):
    _fuchsia_product_assembly(
        name = name + "_product_assembly",
        board_config = board_config,
        product_config = product_config,
        platform_artifacts = platform_artifacts,
        legacy_bundle = legacy_bundle,
        package_validation = package_validation,
    )

    _fuchsia_product_create_system(
        name = name,
        product_assembly = ":" + name + "_product_assembly",
        **kwargs
    )
