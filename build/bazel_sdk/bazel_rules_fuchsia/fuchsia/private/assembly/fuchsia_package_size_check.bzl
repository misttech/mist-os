# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for running size checker on blobfs package."""

load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_inputs")
load(":providers.bzl", "FuchsiaProductImageInfo", "FuchsiaSizeCheckerInfo")

def _fuchsia_package_size_check_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    images_out = ctx.attr.product_image[FuchsiaProductImageInfo].images_out

    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")
    size_report = ctx.actions.declare_file(ctx.label.name + "_size_report.json")
    verbose_output = ctx.actions.declare_file(ctx.label.name + "_verbose_output.json")
    budgets_file = ctx.actions.declare_file(ctx.label.name + "_size_budgets.json")
    size_checker_file = ctx.file.size_checker_file
    product_assembly_out = ctx.attr.product_image[FuchsiaProductImageInfo].product_assembly_out

    # Convert size_checker.json to size_budgets.json
    ctx.actions.run(
        outputs = [budgets_file],
        inputs = [size_checker_file, product_assembly_out],
        executable = ctx.executable._convert_size_limits,
        arguments = [
            "--size-limits",
            size_checker_file.path,
            "--image-assembly-config",
            product_assembly_out.path + "/image_assembly.json",
            "--output",
            budgets_file.path,
            "--max-blob-contents-size",
            str(ctx.attr.max_blob_contents_size),
        ],
    )

    # Size checker execution
    inputs = get_ffx_assembly_inputs(fuchsia_toolchain) + [budgets_file] + ctx.files.product_image
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
        "--blob-sizes",
        images_out.path + "/blobs.json",
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

    return [
        DefaultInfo(files = depset(direct = outputs)),
        FuchsiaSizeCheckerInfo(
            size_budgets = budgets_file,
            size_report = size_report,
            verbose_output = verbose_output,
        ),
    ]

fuchsia_package_size_check = rule(
    doc = """Create a size summary for blobfs packages.""",
    implementation = _fuchsia_package_size_check_impl,
    provides = [FuchsiaSizeCheckerInfo],
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "size_checker_file": attr.label(
            doc = "Blobfs size budget file. It will later be converted to size_budgets.json file",
            allow_single_file = True,
            mandatory = True,
        ),
        "product_image": attr.label(
            doc = "fuchsia_product_image target to check size",
            providers = [FuchsiaProductImageInfo],
        ),
        "blobfs_capacity": attr.int(
            doc = "Total Capacity of BlobFS. Deprecated!",
        ),
        "max_blob_contents_size": attr.int(
            doc = "Total size of BlobFS Contents",
            mandatory = True,
        ),
        "_convert_size_limits": attr.label(
            default = "//fuchsia/tools:convert_size_limits",
            executable = True,
            cfg = "exec",
        ),
    },
)
