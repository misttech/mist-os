# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Build information used in the Bazel product configs."""

load("@legacy_ninja_build_outputs//:build_args.bzl", "build_info_product")

DEFAULT_PRODUCT_BUILD_INFO = {
    "name": build_info_product,
    "version": "LABEL(@legacy_ninja_build_outputs//:build_info_version.txt)",
    "jiri_snapshot": "LABEL(@legacy_ninja_build_outputs//:jiri_snapshot.xml)",
    "latest_commit_date": "LABEL(@legacy_ninja_build_outputs//:latest-commit-date.txt)",
    "minimum_utc_stamp": "LABEL(@legacy_ninja_build_outputs//:minimum-utc-stamp.txt)",
}

RECOVERY_PRODUCT_BUILD_INFO = {
    "name": build_info_product + "_recovery",
    "version": "LABEL(@legacy_ninja_build_outputs//:build_info_version.txt)",
    "jiri_snapshot": "LABEL(@legacy_ninja_build_outputs//:jiri_snapshot.xml)",
    "latest_commit_date": "LABEL(@legacy_ninja_build_outputs//:latest-commit-date.txt)",
    "minimum_utc_stamp": "LABEL(@legacy_ninja_build_outputs//:minimum-utc-stamp.txt)",
}
