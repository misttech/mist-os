# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":fuchsia_task_flash.bzl", "fuchsia_task_flash")
load(":fuchsia_task_verbs.bzl", "verbs")

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
