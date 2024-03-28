# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Build information used in the Bazel product configs."""

load("@fuchsia_build_info//:build_args.bzl", "build_info_product")

DEFAULT_PRODUCT_BUILD_INFO = {
    "name": build_info_product,
    "version": "LABEL(@fuchsia_build_info//:build_info_version.txt)",
    "jiri_snapshot": "LABEL(@fuchsia_build_info//:jiri_snapshot.xml)",
    "latest_commit_date": "LABEL(@fuchsia_build_info//:latest-commit-date.txt)",
    "minimum_utc_stamp": "LABEL(@fuchsia_build_info//:minimum-utc-stamp.txt)",
}
