# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Flash device using product bundle as a task workflow."""

load(":fuchsia_task_ffx.bzl", "ffx_task_rule")
load(":providers.bzl", "FuchsiaProductBundleInfo")

def _fuchsia_task_repo_add_impl(ctx, _make_ffx_task):
    return _make_ffx_task(
        prepend_args = [
            "repository",
            "add",
            ctx.attr.product_bundle[FuchsiaProductBundleInfo].product_bundle,
            "--prefix",
            ctx.attr.package_repository_prefix,
        ],
    )

_fuchsia_task_repo_add, _fuchsia_task_repo_add_for_test, fuchsia_task_repo_add = ffx_task_rule(
    doc = """Start package repository from product bundle.""",
    implementation = _fuchsia_task_repo_add_impl,
    attrs = {
        "product_bundle": attr.label(
            doc = "Product bundle which contains repository",
            providers = [FuchsiaProductBundleInfo],
            mandatory = True,
        ),
        "package_repository_prefix": attr.string(
            doc = "Prefix of package repository",
            default = "devhost",
        ),
    },
)
