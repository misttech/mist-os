# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Soft Reboot test."""

import logging

from fuchsia_base_test import fuchsia_base_test
from honeydew.fuchsia_device import fuchsia_device
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class SoftRebootTest(fuchsia_base_test.FuchsiaBaseTest):
    """Soft Reboot test.

    Attributes:
        dut: FuchsiaDevice object.

    Required Mobly Test Params:
        num_reboots (int): Number of times reboot test need to be executed.
    """

    def pre_run(self) -> None:
        """Mobly method used to generate the test cases at run time."""
        test_arg_tuple_list: list[tuple[int]] = []

        for iteration in range(1, int(self.user_params["num_reboots"]) + 1):
            test_arg_tuple_list.append((iteration,))

        self.generate_tests(
            test_logic=self._test_logic,
            name_func=self._name_func,
            arg_sets=test_arg_tuple_list,
        )

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns dut variable with FuchsiaDevice object
        """
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def _test_logic(self, iteration: int) -> None:
        """Test case logic that."""
        _LOGGER.info("Starting the Soft Reboot test iteration# %s", iteration)
        self.dut.reboot()
        _LOGGER.info(
            "Successfully ended the Soft Reboot test iteration# %s", iteration
        )

    def _name_func(self, iteration: int) -> str:
        """This function generates the names of each test case based on each
        argument set.

        The name function should have the same signature as the actual test
        logic function.

        Returns:
            Test case name
        """
        return f"test_soft_reboot_{iteration}"


if __name__ == "__main__":
    test_runner.main()
