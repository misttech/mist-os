# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia platform shortcuts."""

# TODO document
fuchsia_platforms = struct(
    arm64 = "@fuchsia_sdk//fuchsia/constraints/platforms:fuchsia_arm64",
    riscv64 = "@fuchsia_sdk//fuchsia/constraints/platforms:fuchsia_riscv64",
    x64 = "@fuchsia_sdk//fuchsia/constraints/platforms:fuchsia_x64",
)

# The list of supported Fuchsia platforms
ALL_SUPPORTED_PLATFORMS = [
    fuchsia_platforms.arm64,
    fuchsia_platforms.riscv64,
    fuchsia_platforms.x64,
]
