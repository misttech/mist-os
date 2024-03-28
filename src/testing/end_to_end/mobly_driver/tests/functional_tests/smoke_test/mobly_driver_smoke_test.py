#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly Driver smoke test.

This E2E smoke test exercises the Mobly Driver. Specifically, it tests the
following functionalities:

1) Execution-environment dependent logic.
2) Device config generation.
3) Mobly test execution.

The successful execution of this test in Fuchsia infra also proves the following
infra integration points:
1) Test parsing.
2) Test artifact reporting.
"""
import os

from mobly import asserts
from mobly import base_test
from mobly import test_runner

import fuchsia_device


class MoblyDriverSmokeTest(base_test.BaseTestClass):
    """Mobly Driver smoke tests."""

    def test_mobly_controller_init(self) -> None:
        """Test case to ensure Mobly controller initializes successfully"""
        # Use Mobly's built-in register_controller() to parse configs and create
        # devices controllers for test usage.
        fuchsia_devices = self.register_controller(fuchsia_device)
        asserts.assert_true(
            fuchsia_devices, "Expect at least 1 created controller."
        )

    def test_params_exist(self) -> None:
        """Test case to ensure test params are included in the Mobly config"""
        asserts.assert_true(self.user_params, "Test params are missing.")

    def test_output_dir(self) -> None:
        """Test case to ensure Mobly output dir can be written to"""
        test_artifact_path = os.path.join(self.log_path, "artifact.txt")
        with open(test_artifact_path, "w+", encoding="utf8") as file_handle:
            file_handle.write("data")


if __name__ == "__main__":
    test_runner.main()
