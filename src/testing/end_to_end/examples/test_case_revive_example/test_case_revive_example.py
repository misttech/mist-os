# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Example usage of test_case_revive.TestCaseRevive."""

import logging

from honeydew.fuchsia_device.fuchsia_device import FuchsiaDevice
from mobly import test_runner
from test_case_revive import test_case_revive

dut: FuchsiaDevice

_LOGGER: logging.Logger = logging.getLogger(__name__)


def _to_run_before_test(foo: int) -> None:
    _LOGGER.info(
        "This line should get printed at the beginning of the revived test. DUT name is: %s, foo = %s.",
        dut.device_name,
        foo,
    )


def _to_run_after_test(bar: int) -> None:
    _LOGGER.info(
        "This line should get printed at the end of the revived test. DUT name is: %s, bar = %s",
        dut.device_name,
        bar,
    )


class ExampleTestCaseRevive(test_case_revive.TestCaseRevive):
    """Example usage of test_case_revive.TestCaseRevive."""

    def setup_class(self) -> None:
        super().setup_class()

        global dut
        dut = self.fuchsia_devices[0]

    @test_case_revive.tag_test(tag_name="revive_test_case")
    def test_firmware_version(self) -> None:
        """This test will be run both as a normal test and also as a revived.

        For the revived test case, sequence will be:
            1. Run this test case method
            2. Perform `fuchsia_device_operation` operation
            3. Re-run this test case method
        """
        for fuchsia_device in self.fuchsia_devices:
            _LOGGER.info(
                "%s is running on %s firmware",
                fuchsia_device.device_name,
                fuchsia_device.firmware_version,
            )

    @test_case_revive.tag_test(
        tag_name="revive_test_case",
        test_method_execution_frequency=test_case_revive.TestMethodExecutionFrequency.POST_ONLY,
        pre_test_execution_fn=_to_run_before_test,
        pre_test_execution_fn_kwargs={"foo": 1},
        post_test_execution_fn=_to_run_after_test,
        post_test_execution_fn_kwargs={"bar": 1},
    )
    def _test_run_only_with_revive_option_with_pre_and_post_test_execution_fn_and_kwargs(
        self,
    ) -> None:
        """This will be run only as a revived test case and runs the following
        sequence:

            1. Run _to_run_before_test method
            2. Perform `fuchsia_device_operation` operation
            3. Run this test case method
            4. Run _to_run_after_test method
        """
        for fuchsia_device in self.fuchsia_devices:
            _LOGGER.info(
                "%s is a %s board",
                fuchsia_device.device_name,
                fuchsia_device.board,
            )


if __name__ == "__main__":
    test_runner.main()
