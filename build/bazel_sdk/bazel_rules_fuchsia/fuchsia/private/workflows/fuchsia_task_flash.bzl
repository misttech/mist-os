# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Flash device using product bundle as a task workflow."""

load(":providers.bzl", "FuchsiaProductBundleInfo")
load(":fuchsia_task_ffx.bzl", "ffx_task_rule")

def _fuchsia_task_flash_impl(ctx, _make_ffx_task):
    return _make_ffx_task(
        prepend_args = [
            "target",
            "flash",
            "--product-bundle",
            ctx.attr.product_bundle[FuchsiaProductBundleInfo].product_bundle,
        ],
    )

_fuchsia_task_flash, _fuchsia_task_flash_for_test, fuchsia_task_flash = ffx_task_rule(
    doc = """Flash device using product bundle.""",
    implementation = _fuchsia_task_flash_impl,
    attrs = {
        "product_bundle": attr.label(
            doc = "Product bundle that is needed to flash the device",
            providers = [FuchsiaProductBundleInfo],
            mandatory = True,
        ),
    },
)
