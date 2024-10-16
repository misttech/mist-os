# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":fuchsia_task_download.bzl", "fuchsia_task_download")
load(":fuchsia_task_emu.bzl", "fuchsia_task_emu")
load(":fuchsia_task_ffx.bzl", "fuchsia_task_ffx")
load(":fuchsia_task_flash.bzl", "fuchsia_task_flash")
load(":fuchsia_task_repo_add.bzl", "fuchsia_task_repo_add")
load(":fuchsia_task_verbs.bzl", "make_help_executable", "verbs")
load(":fuchsia_workflow.bzl", "fuchsia_workflow")

def product_bundles_help_executable(ctx, is_remote = False):
    """Makes a help message for a product bundle target.

    Args:
        ctx: A rule ctx.
        is_remote: Whether the product bundle is remote.
    Returns:
        An executable that can be added to DefaultInfo.
    """
    options = {
        verbs.emu: "Starts an emulator with the product bundle.",
        verbs.flash: "Flashes a device with the product bundle.",
    }
    if is_remote:
        options |= {
            verbs.download: "Downloads the product bundle.",
        }
    else:
        options |= {
            verbs.ota: "Runs ota on a device with the product bundle.",
            verbs.zip: "Creates a zipped version of the product bundle.",
        }
    return make_help_executable(ctx, options)

def _maybe_download_first(
        *,
        name,
        task_type,
        download_task,
        testonly = None,
        visibility = None,
        tags = [],
        **kwargs):
    if download_task:
        task_type(
            name = name + "_standalone",
            testonly = testonly,
            visibility = visibility,
            tags = tags + ["manual"],
            **kwargs
        )
        fuchsia_workflow(
            name = name,
            sequence = [
                download_task,
                name + "_standalone",
            ],
            testonly = testonly,
            visibility = visibility,
            tags = tags,
        )
    else:
        task_type(
            name = name,
            testonly = testonly,
            visibility = visibility,
            tags = tags,
            **kwargs
        )

# buildifier: disable=function-docstring
def fuchsia_product_bundle_tasks(
        *,
        name,
        product_bundle,
        is_remote = False,
        **kwargs):
    name = name.replace("_tasks", "")

    # For `bazel run :product_bundle.download`
    download_task = None
    if is_remote:
        download_task = verbs.download(name)
        fuchsia_task_download(
            name = download_task,
            product_bundle = product_bundle,
            **kwargs
        )

    # For `bazel run :product_bundle.emu`
    _maybe_download_first(
        name = verbs.emu(name),
        task_type = fuchsia_task_emu,
        download_task = download_task,
        product_bundle = product_bundle,
        default_argument_scope = "global",
        **kwargs
    )

    # For `bazel run :product_bundle.flash`
    _maybe_download_first(
        name = verbs.flash(name),
        task_type = fuchsia_task_flash,
        download_task = download_task,
        product_bundle = product_bundle,
        **kwargs
    )

    # For `bazel run :product_bundle.ota`
    if not is_remote:
        package_repository_prefix = "devhost"
        repo_add_task = verbs.repo_add(name)
        fuchsia_task_repo_add(
            name = repo_add_task,
            product_bundle = product_bundle,
            package_repository_prefix = package_repository_prefix,
            **kwargs
        )

        set_channel_task = verbs.set_channel(name)
        fuchsia_task_ffx(
            name = set_channel_task,
            arguments = [
                "target",
                "update",
                "channel",
                "set",
                "%s.fuchsia.com" % package_repository_prefix,
            ],
            **kwargs
        )

        check_now_task = verbs.check_now(name)
        fuchsia_task_ffx(
            name = check_now_task,
            arguments = [
                "target",
                "update",
                "check-now",
                "--monitor",
            ],
            **kwargs
        )

        ota_task = verbs.ota(name)
        fuchsia_workflow(
            name = ota_task,
            sequence = [
                repo_add_task,
                set_channel_task,
                check_now_task,
            ],
            **kwargs
        )
