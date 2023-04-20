# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for running size checker on given package."""

load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_inputs")

def _fuchsia_package_size_check_impl(ctx):
    budgets_file = ctx.file.budgets_file
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]

    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")
    size_report = ctx.actions.declare_file(ctx.label.name + "_size_report.json")
    verbose_output = ctx.actions.declare_file(ctx.label.name + "_verbose_output.json")

    inputs = get_ffx_assembly_inputs(fuchsia_toolchain) + [budgets_file]
    outputs = [size_report, verbose_output, ffx_isolate_dir]

    # Gather all the arguments to pass to ffx.
    ffx_invocation = [
        fuchsia_toolchain.ffx.path,
        "--config \"assembly_enabled=true\"",
        "--isolate-dir",
        ffx_isolate_dir.path,
        "assembly",
        "size-check",
        "package",
        "--budgets",
        budgets_file.path,
        "--blobfs-layout",
        "deprecated_padded",
        "--gerrit-output",
        size_report.path,
        "--verbose-json-output",
        verbose_output.path,
    ]

    # We only need blobs configuration for blobfs case.
    if ctx.file.blobs_config_file:
        blobs_config_file = ctx.file.blobs_config_file
        inputs.append(blobs_config_file)
        ffx_invocation += [
            "--blob-sizes",
            blobs_config_file.path,
        ]

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
        progress_message = "Size checking for %s" % ctx.label.name,
    )

    return [DefaultInfo(files = depset(direct = outputs))]

fuchsia_package_size_check = rule(
    doc = """Create a size summary for package.""",
    implementation = _fuchsia_package_size_check_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "budgets_file": attr.label(
            doc = "Blobfs size budget file.",
            allow_single_file = True,
            mandatory = True,
        ),
        "blobs_config_file": attr.label(
            doc = "Blobs config file containing sizes of blobs",
            allow_single_file = True,
        ),
    },
)
