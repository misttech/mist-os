# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Start an emulator using product bundle as a task workflow."""

load(":fuchsia_shell_task.bzl", "shell_task_rule")
load(":fuchsia_task_download.bzl", "get_product_bundle_dir")
load(":providers.bzl", "FuchsiaProductBundleInfo")

def _fuchsia_task_emu_impl(ctx, make_shell_task):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    pb_path = get_product_bundle_dir(ctx.attr.product_bundle[FuchsiaProductBundleInfo])
    return make_shell_task(
        command = [
            sdk.ffx,
            "emu",
            "start",
            pb_path,
        ],
        runfiles = [
            sdk.sdk_manifest,
            sdk.aemu_runfiles,
            sdk.fvm,
            sdk.fvm_manifest,
            sdk.zbi,
            sdk.zbi_manifest,
        ],
    )

_fuchsia_task_emu, _fuchsia_task_emu_for_test, fuchsia_task_emu = shell_task_rule(
    doc = """Start emulator using product bundle.""",
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    implementation = _fuchsia_task_emu_impl,
    attrs = {
        "product_bundle": attr.label(
            doc = "Product bundle that is needed to start the emulator",
            providers = [FuchsiaProductBundleInfo],
            mandatory = True,
        ),
    },
)
