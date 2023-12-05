# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":fuchsia_task_ffx.bzl", "fuchsia_task_ffx")
load(":fuchsia_task_flash.bzl", "fuchsia_task_flash")
load(":fuchsia_task_repo_add.bzl", "fuchsia_task_repo_add")
load(":fuchsia_task_verbs.bzl", "verbs")
load(":fuchsia_workflow.bzl", "fuchsia_workflow")

# buildifier: disable=function-docstring
def fuchsia_product_bundle_tasks(
        *,
        name,
        product_bundle):
    name = name.replace("_tasks", "")

    # For `bazel run :product_bundle.flash`
    flash_task = verbs.flash(name)
    fuchsia_task_flash(
        name = flash_task,
        product_bundle = product_bundle,
    )

    # For `bazel run :product_bundle.ota`
    package_repository_prefix = "devhost"
    repo_add_task = verbs.repo_add(name)
    fuchsia_task_repo_add(
        name = repo_add_task,
        product_bundle = product_bundle,
        package_repository_prefix = package_repository_prefix,
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
    )

    ota_task = verbs.ota(name)
    fuchsia_workflow(
        name = ota_task,
        sequence = [
            repo_add_task,
            set_channel_task,
            check_now_task,
        ],
    )
