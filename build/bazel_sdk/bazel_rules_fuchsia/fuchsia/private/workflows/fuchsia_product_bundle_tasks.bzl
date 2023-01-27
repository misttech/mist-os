# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""ffx product-bundle get invokation as a workflow task."""

load(":providers.bzl", "FuchsiaProductBundleInfo")
load(":fuchsia_task_ffx.bzl", "ffx_task_rule")
load(":utils.bzl", "full_product_bundle_url")

def _fuchsia_task_fetch_product_bundle_impl(ctx, _make_ffx_task):
    pb_info = ctx.attr.product_bundle[FuchsiaProductBundleInfo]
    if not pb_info.is_remote:
        fail("Local product bundles do not need to be fetched.")
    args = [
        "product-bundle",
        "get",
        full_product_bundle_url(ctx, pb_info),
    ]

    if pb_info.repository:
        args.extend([
            "--repository",
            pb_info.repository,
        ])

    if ctx.attr.force_repository_creation:
        args.append("--force-repo")

    return _make_ffx_task(
        prepend_args = args,
    )

(
    _fuchsia_task_fetch_product_bundle,
    _fuchsia_task_fetch_product_bundle_for_test,
    fuchsia_task_fetch_product_bundle,
) = ffx_task_rule(
    doc = """Fetches a remote product bundle.""",
    implementation = _fuchsia_task_fetch_product_bundle_impl,
    attrs = {
        "product_bundle": attr.label(
            doc = "Product bundle to fetch.",
            providers = [[FuchsiaProductBundleInfo]],
            mandatory = True,
        ),
        "force_repository_creation": attr.bool(
            doc = """If True, will pass --force-repo causing forcing the package
            repository creation even if it already exists.
            """,
            default = True,
        ),
    },
)

def _fuchsia_task_remove_product_bundle_impl(ctx, _make_ffx_task):
    pb_info = ctx.attr.product_bundle[FuchsiaProductBundleInfo]
    args = [
        "product-bundle",
        "remove",
        full_product_bundle_url(ctx, pb_info),
        "--force",
    ]

    return _make_ffx_task(
        prepend_args = args,
    )

(
    _fuchsia_task_remove_product_bundle,
    _fuchsia_task_remove_product_bundle_for_test,
    fuchsia_task_remove_product_bundle,
) = ffx_task_rule(
    doc = """Removes a downloaded product bundle.""",
    implementation = _fuchsia_task_remove_product_bundle_impl,
    attrs = {
        "product_bundle": attr.label(
            doc = "Product bundle to fetch.",
            providers = [[FuchsiaProductBundleInfo]],
            mandatory = True,
        ),
    },
)
