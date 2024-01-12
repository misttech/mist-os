# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for running size checker on product and package."""

load(
    "@fuchsia_sdk//fuchsia/private/assembly:fuchsia_package_size_check.bzl",
    "fuchsia_package_size_check",
)
load(
    "@fuchsia_sdk//fuchsia/private/assembly:fuchsia_product_size_check.bzl",
    "fuchsia_product_size_check",
)
load(
    "@fuchsia_sdk//fuchsia/private/assembly:fuchsia_size_report_aggregator.bzl",
    "fuchsia_size_report_aggregator",
)

def fuchsia_size_checker(
        name,
        product_image = None,
        # Deprecated
        update_package = None,
        size_checker_file = None,
        blobfs_capacity = None,
        max_blob_contents_size = None,
        blobfs_creep_limit = 102400):
    """An implementation of size checker that run product size checker, blobfs package size checker and non-blobfs size chekcer. It will also aggregate all the reports and create a merged report.

    Args:
        name: Name of the rule.
        product_image: fuchsia_product_image target to check size.
        size_checker_file: "Blobfs size budget file. It will later be converted to size_budgets.json file.
        blobfs_capacity: Total Capacity of BlobFS.
        max_blob_contents_size: Total size of BlobFS contents.
        blobfs_creep_limit: Creep limit for Blobfs, this is how much BlobFS contents can increase in one CL.
    """
    fuchsia_product_size_check(
        name = name + "_product",
        product_image = product_image,
        blobfs_creep_limit = blobfs_creep_limit,
    )

    fuchsia_package_size_check(
        name = name + "_package_blobfs",
        size_checker_file = size_checker_file,
        product_image = product_image,
        blobfs_capacity = blobfs_capacity,
        max_blob_contents_size = max_blob_contents_size,
    )

    fuchsia_size_report_aggregator(
        name = name + "_aggregator",
        size_reports = [
            ":" + name + "_package_blobfs",
            ":" + name + "_product",
        ],
    )
