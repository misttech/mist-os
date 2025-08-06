# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Power Cycle test."""

import logging

from fuchsia_base_test import fuchsia_base_test
from honeydew.auxiliary_devices.power_switch import power_switch
from honeydew.fuchsia_device import fuchsia_device
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


_DMC_MODULE: str = (
    "honeydew.auxiliary_devices.power_switch.power_switch_using_dmc"
)
_DMC_CLASS: str = "PowerSwitchUsingDmc"


class PowerCycleTest(fuchsia_base_test.FuchsiaBaseTest):
    """power cycle test.

    Attributes:
        dut: FuchsiaDevice object.

    Required Mobly Test Params:
        num_power_cycles (int): Number of times power_cycle test need to be
            executed.
    """

    def pre_run(self) -> None:
        """Mobly method used to generate the test cases at run time."""
        test_arg_tuple_list: list[tuple[int]] = []

        for iteration in range(
            1, int(self.user_params["num_power_cycles"]) + 1
        ):
            test_arg_tuple_list.append((iteration,))

        self.generate_tests(
            test_logic=self._test_logic,
            name_func=self._name_func,
            arg_sets=test_arg_tuple_list,
        )

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self._power_switch: power_switch.PowerSwitch
        self._outlet: int | None
        (self._power_switch, self._outlet) = self._lookup_power_switch(self.dut)

    def _test_logic(self, iteration: int) -> None:
        """Test case logic that performs power cycle of fuchsia device."""
        _LOGGER.info("Starting the Power Cycle test iteration# %s", iteration)
        self.dut.power_cycle(
            power_switch=self._power_switch, outlet=self._outlet
        )
        _LOGGER.info(
            "Successfully ended the Power Cycle test iteration# %s", iteration
        )

    def _name_func(self, iteration: int) -> str:
        """This function generates the names of each test case based on each
        argument set.

        The name function should have the same signature as the actual test
        logic function.

        Returns:
            Test case name
        """
        return f"test_power_cycle_{iteration}"


if __name__ == "__main__":
    test_runner.main()
