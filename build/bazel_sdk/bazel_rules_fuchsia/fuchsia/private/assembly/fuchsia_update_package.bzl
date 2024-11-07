# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for creating an update package."""

load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_args", "get_ffx_assembly_inputs")
load(
    ":providers.bzl",
    "FuchsiaPartitionsConfigInfo",
    "FuchsiaProductImageInfo",
    "FuchsiaUpdatePackageInfo",
)
load(":utils.bzl", "LOCAL_ONLY_ACTION_KWARGS")

def _fuchsia_update_package_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    partitions_configuration = ctx.attr.partitions_config[FuchsiaPartitionsConfigInfo]
    system_a_out = ctx.attr.main[FuchsiaProductImageInfo].images_out

    out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")

    inputs = get_ffx_assembly_inputs(fuchsia_toolchain) + [partitions_configuration.files, ctx.file.update_version_file] + ctx.files.main
    outputs = [out_dir, ffx_isolate_dir]

    # Gather all the arguments to pass to ffx.
    ffx_invocation = get_ffx_assembly_args(fuchsia_toolchain) + [
        "--isolate-dir",
        ffx_isolate_dir.path,
        "assembly",
        "create-update",
        "--partitions",
        partitions_configuration.config.path,
        "--board-name",
        ctx.attr.board_name,
        "--version-file",
        ctx.file.update_version_file.path,
        "--epoch",
        ctx.attr.update_epoch,
        "--outdir",
        out_dir.path,
        "--system-a",
        system_a_out.path + "/images.json",
    ]

    if ctx.attr.recovery:
        system_r_out = ctx.attr.recovery[FuchsiaProductImageInfo].images_out
        ffx_invocation += [
            "--system-r",
            system_r_out.path + "/images.json",
        ]
        inputs += ctx.files.recovery

    script_lines = [
        "set -e",
        "mkdir -p " + ffx_isolate_dir.path,
        " ".join(ffx_invocation),
    ]
    script = "\n".join(script_lines)

    ctx.actions.run_shell(
        inputs = inputs,
        outputs = outputs,
        command = script,
        progress_message = "Create update package for %s" % ctx.label.name,
        **LOCAL_ONLY_ACTION_KWARGS
    )
    return [
        DefaultInfo(files = depset(direct = outputs + inputs)),
        OutputGroupInfo(
            debug_files = depset([ffx_isolate_dir]),
            all_files = depset([out_dir, ffx_isolate_dir] + inputs),
        ),
        FuchsiaUpdatePackageInfo(
            update_out = out_dir,
        ),
    ]

fuchsia_update_package = rule(
    doc = """Declares a Fuchsia update package.""",
    implementation = _fuchsia_update_package_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    provides = [FuchsiaUpdatePackageInfo],
    attrs = {
        "main": attr.label(
            doc = "fuchsia product to put in slot A.",
            providers = [FuchsiaProductImageInfo],
        ),
        "recovery": attr.label(
            doc = "fuchsia product to put in slot R.",
            providers = [FuchsiaProductImageInfo],
        ),
        "board_name": attr.string(
            doc = "Name of the board this update package runs on. E.g. qemu-x64.",
            mandatory = True,
        ),
        "partitions_config": attr.label(
            doc = "Partitions config to use.",
            mandatory = True,
        ),
        "update_version_file": attr.label(
            doc = "Version file needed to create update package.",
            allow_single_file = True,
            mandatory = True,
        ),
        "update_epoch": attr.string(
            doc = "Epoch needed to create update package.",
            mandatory = True,
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)
