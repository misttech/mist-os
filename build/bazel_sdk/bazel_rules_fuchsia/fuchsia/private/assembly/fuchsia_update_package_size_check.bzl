# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for running size checker on non-blobfs package."""

load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_inputs")
load(":providers.bzl", "FuchsiaSizeCheckerInfo", "FuchsiaUpdatePackageInfo")

UPDATE_BUDGET_TEMPLATE = """{
    "package_set_budgets": [
        {
            "name": "Update Package",
            "budget_bytes": %s,
            "creep_budget_bytes": %s,
            "merge": false,
            "packages": [
                "%s",
            ],
        }
    ]
}
"""

def _fuchsia_update_package_size_check_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    update_out = ctx.attr.update_package[FuchsiaUpdatePackageInfo].update_out

    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")
    size_report = ctx.actions.declare_file(ctx.label.name + "_size_report.json")
    verbose_output = ctx.actions.declare_file(ctx.label.name + "_verbose_output.json")
    budgets_file = ctx.actions.declare_file(ctx.label.name + "_budgets.json")

    update_package_budget = UPDATE_BUDGET_TEMPLATE % (
        str(ctx.attr.blobfs_capacity - 2 * ctx.attr.max_blob_contents_size),
        str(ctx.attr.update_package_size_creep_limit),
        update_out.path + "/update_package_manifest.json",
    )

    ctx.actions.write(budgets_file, update_package_budget)

    # Size checker execution
    inputs = get_ffx_assembly_inputs(fuchsia_toolchain) + [budgets_file] + ctx.files.update_package
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
        DefaultInfo(files = depset(direct = [size_report, verbose_output])),
        FuchsiaSizeCheckerInfo(
            size_report = size_report,
            verbose_output = verbose_output,
        ),
    ]

fuchsia_update_package_size_check = rule(
    doc = """Create a size summary for non-blobfs packages.""",
    implementation = _fuchsia_update_package_size_check_impl,
    provides = [FuchsiaSizeCheckerInfo],
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "update_package": attr.label(
            doc = "fuchsia_update_package target to check size",
            providers = [FuchsiaUpdatePackageInfo],
        ),
        "blobfs_capacity": attr.int(
            doc = "Total Capacity of BlobFS",
            mandatory = True,
        ),
        "max_blob_contents_size": attr.int(
            doc = "Total size of BlobFS Contents",
            mandatory = True,
        ),
        "update_package_size_creep_limit": attr.int(
            doc = "Size allowed to change in singel CL",
            mandatory = True,
        ),
    },
)
