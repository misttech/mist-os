# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Example usage of test_case_revive.TestCaseRevive."""

import logging

from test_case_revive import test_case_revive
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class ExampleTestCaseRevive(test_case_revive.TestCaseRevive):
    """Example usage of test_case_revive.TestCaseRevive."""

    @test_case_revive.tag_test(tag_name="revive_test_case")
    def test_firmware_version(self) -> None:
        """This test will be run both as a normal test and also as a revived."""
        for fuchsia_device in self.fuchsia_devices:
            _LOGGER.info(
                "%s is running on %s firmware",
                fuchsia_device.device_name,
                fuchsia_device.firmware_version,
            )

    @test_case_revive.tag_test(
        tag_name="revive_test_case",
        fuchsia_device_operation=test_case_revive.FuchsiaDeviceOperation.SOFT_REBOOT,
        test_method_execution_frequency=test_case_revive.TestMethodExecutionFrequency.POST_ONLY,
    )
    def _test_run_only_with_revive_option(self) -> None:
        """This will be run only as a revived test case and runs the following
        sequence:

            1. Perform soft reboot operation
            2. Run this test method
        """
        for fuchsia_device in self.fuchsia_devices:
            _LOGGER.info(
                "%s is a %s board",
                fuchsia_device.device_name,
                fuchsia_device.board,
            )


if __name__ == "__main__":
    test_runner.main()
