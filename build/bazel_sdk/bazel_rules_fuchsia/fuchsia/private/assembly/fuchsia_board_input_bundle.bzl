# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules for defining assembly board input bundle."""

load("@bazel_skylib//lib:paths.bzl", "paths")
load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_inputs")
load("//fuchsia/private:fuchsia_package.bzl", "get_driver_component_manifests")
load("//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")
load(
    ":providers.bzl",
    "FuchsiaBoardInputBundleInfo",
)

def _fuchsia_board_input_bundle_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    driver_entries = []
    creation_inputs = []
    for dep in ctx.attr.base_driver_packages:
        driver_entries.append(
            {
                "package": dep[FuchsiaPackageInfo].package_manifest.path,
                "components": get_driver_component_manifests(dep),
                "set": "base",
            },
        )
        creation_inputs += dep[FuchsiaPackageInfo].files
    for dep in ctx.attr.bootfs_driver_packages:
        driver_entries.append(
            {
                "package": dep[FuchsiaPackageInfo].package_manifest.path,
                "components": get_driver_component_manifests(dep),
                "set": "bootfs",
            },
        )
        creation_inputs += dep[FuchsiaPackageInfo].files

    # Create driver list file
    driver_list = {"drivers": driver_entries}
    driver_list_file = ctx.actions.declare_file(ctx.label.name + "_driver.list")
    creation_inputs.append(driver_list_file)
    content = json.encode_indent(driver_list, indent = "  ")
    ctx.actions.write(driver_list_file, content)

    creation_args = ["--drivers", driver_list_file.path]

    # Add configs
    for (arg, file) in [
        ("--cpu-manager-config", "cpu_manager_config"),
        ("--power-manager-config", "power_manager_config"),
        ("--power-metrics-recorder-config", "power_metrics_recorder_config"),
        ("--thermal-config", "thermal_config"),
    ]:
        if not getattr(ctx.file, file):
            continue
        creation_inputs.append(getattr(ctx.file, file))
        creation_args.extend(
            [
                arg,
                getattr(ctx.file, file).path,
            ],
        )

    # Add package entries
    for dep in ctx.attr.base_packages:
        creation_args.extend(
            [
                "--base-packages",
                dep[FuchsiaPackageInfo].package_manifest.path,
            ],
        )
        creation_inputs += dep[FuchsiaPackageInfo].files

    for dep in ctx.attr.bootfs_packages:
        creation_args.extend(
            [
                "--bootfs-packages",
                dep[FuchsiaPackageInfo].package_manifest.path,
            ],
        )
        creation_inputs += dep[FuchsiaPackageInfo].files

    # Create Board Input Bundle
    ffx_tool = fuchsia_toolchain.ffx
    board_input_bundle_json = ctx.actions.declare_file(ctx.label.name + "_out/board_input_bundle.json")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")
    ffx_invocation = [
        ffx_tool.path,
        "--config \"assembly_enabled=true,sdk.root=" + ctx.attr._sdk_manifest.label.workspace_root + "\"",
        "--isolate-dir " + ffx_isolate_dir.path,
        "assembly",
        "board-input-bundle",
        "--outdir",
        board_input_bundle_json.dirname,
    ] + creation_args

    script_lines = [
        "set -e",
        "mkdir -p " + ffx_isolate_dir.path,
        " ".join(ffx_invocation),
    ]
    script = "\n".join(script_lines)
    ctx.actions.run_shell(
        inputs = creation_inputs + get_ffx_assembly_inputs(fuchsia_toolchain),
        outputs = [board_input_bundle_json, ffx_isolate_dir],
        command = script,
        progress_message = "Creating board input bundle for %s" % ctx.label.name,
    )

    deps = [board_input_bundle_json] + creation_inputs

    return [
        DefaultInfo(
            files = depset(
                direct = deps,
            ),
        ),
        FuchsiaBoardInputBundleInfo(
            config = board_input_bundle_json,
            files = deps,
        ),
    ]

fuchsia_board_input_bundle = rule(
    doc = """Generates a board input bundle.""",
    implementation = _fuchsia_board_input_bundle_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "base_driver_packages": attr.label_list(
            doc = "Base-driver packages to include in board.",
            providers = [FuchsiaPackageInfo],
            default = [],
        ),
        "bootfs_driver_packages": attr.label_list(
            doc = "Bootfs-driver packages to include in board.",
            providers = [FuchsiaPackageInfo],
            default = [],
        ),
        "base_packages": attr.label_list(
            doc = "Base packages to include in board.",
            providers = [FuchsiaPackageInfo],
            default = [],
        ),
        "bootfs_packages": attr.label_list(
            doc = "Bootfs packages to include in board.",
            providers = [FuchsiaPackageInfo],
            default = [],
        ),
        "cpu_manager_config": attr.label(
            doc = "Path to cpu_manager configuration",
            allow_single_file = True,
        ),
        "power_manager_config": attr.label(
            doc = "Path to power_manager configuration",
            allow_single_file = True,
        ),
        "power_metrics_recorder_config": attr.label(
            doc = "Path to power_metrics_recorder configuration",
            allow_single_file = True,
        ),
        "thermal_config": attr.label(
            doc = "Path to thermal configuration",
            allow_single_file = True,
        ),
        "_sdk_manifest": attr.label(
            allow_single_file = True,
            default = "@fuchsia_sdk//:meta/manifest.json",
        ),
    },
)

def _fuchsia_prebuilt_board_input_bundle_impl(ctx):
    return [
        DefaultInfo(
            files = depset(
                direct = [ctx.file.config],
            ),
        ),
        FuchsiaBoardInputBundleInfo(
            config = ctx.file.config,
            files = ctx.files.files,
        ),
    ]

def fuchsia_prebuilt_board_input_bundle(name, config, files = []):
    """Define a Board Input Bundle based on pre-existed BIB directory.

    Args:
        name: Name for fuchsia_prebuilt_board_input_bundle target.
        config: Path to Board Input Bundle config file.
        files: Supporting files needed for this target
    """
    filegroup = "{}_files".format(name)

    # If the files are not provided, we will glob the directory based on the
    # config file.
    if not files:
        native.filegroup(
            name = filegroup,
            srcs = native.glob(["{}/**/*".format(paths.dirname(config))]),
        )
        files = [":{}".format(filegroup)]

    _fuchsia_prebuilt_board_input_bundle(
        name = name,
        config = config,
        files = files,
    )

_fuchsia_prebuilt_board_input_bundle = rule(
    doc = """Generates a Board Input Bundle based on pre-existed BIB directory.""",
    implementation = _fuchsia_prebuilt_board_input_bundle_impl,
    attrs = {
        "config": attr.label(
            doc = "Path to Board Input Bundle config file.",
            allow_single_file = True,
            mandatory = True,
        ),
        "files": attr.label_list(
            doc = "All files belong to Board Input Bundles",
            default = [],
        ),
    },
)
