# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Contains feature-flags which allow us to toggle experimental changes."""

# Enabled test orchestration experiments/features.
#
# These experiments are configured as "negative experiments" to make it easier
# to rollback experiments on individual test group targets.
#
# Once all of these experiments are removed/disabled, orchestrate will be
# fully integrated.
ENABLED_EXPERIMENTS = [
    # Don't use orchestrate for host test groups.
    "no-orchestrate-host",

    # Don't use orchestrate for emulator test groups.
    "no-orchestrate-emulator",

    # Don't use orchestrate for hardware test groups.
    "no-orchestrate-hardware",

    # Controls whether the subrunner is involved with emulator provisioning.
    # Only applicable for emulator test groups.
    "subrunner-emulator-provisioning",

    # Controls whether several XDG-related environment variables are set by the
    # subrunner.
    "subrunner-xdg-setup",

    # Controls whether the subrunner sets the FFX_ISOLATE_DIR environment
    # variable.
    "subrunner-isolation-setup",

    # Controls whether the subrunner intializes an ffx repository before running
    # tests.
    "subrunner-repository-setup",
]
