#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia base hybrid test class.

A hyrbid test is one that consists of both host-side and target-side components.

An example is a Trace-based performance test:
(host) Initialize Honeydew device controllers
(host) Launch hermetic target-side test via `ffx test`
(target) Execute test
(host) Collect and post-process test output
"""

import logging
import subprocess
import tempfile

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaHybridBaseTest(fuchsia_base_test.FuchsiaBaseTest):
    """Fuchsia hybrid base test class.

    Single device hybrid test.

    Attributes:
        fuchsia_devices: List of FuchsiaDevice objects.
        test_case_path: Directory pointing to a specific test case artifacts.
        snapshot_on: `snapshot_on` test param value converted into SnapshotOn
            Enum.

    Required Mobly Test Params:
        ffx_test_options(list[str]): Test options to supply to `ffx test run`
        ffx_test_url (str): Test URL to execute via `ffx test run`
        timeout_sec (int): Test timeout.
    """

    def setup_class(self) -> None:
        """setup_class is called once before running the testsuite."""
        super().setup_class()

        self.ffx_test_options: list[str] = self.user_params["ffx_test_options"]
        self.ffx_test_url: str = self.user_params["ffx_test_url"]
        self.timeout_sec: int = self.user_params["timeout_sec"]

        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_launch_hermetic_test(self) -> None:
        """Test to launch hermetic Fuchsia test and process test artifacts."""

        # Execute `ffx test` cmd in isolation mode.
        # Note: Avoid using `ffx` transport in Honeydew as it may be deprecated.
        with tempfile.TemporaryDirectory() as iso_dir:
            cmd = [
                "ffx", "--isolate-dir", iso_dir, "-t", self.dut.device_name,
                "test", "run", self.ffx_test_url
            ] + self.ffx_test_options
            output = subprocess.check_output(
                cmd, timeout=self.timeout_sec,
                stderr=subprocess.STDOUT).decode()
            logging.info(output)


if __name__ == "__main__":
    test_runner.main()
