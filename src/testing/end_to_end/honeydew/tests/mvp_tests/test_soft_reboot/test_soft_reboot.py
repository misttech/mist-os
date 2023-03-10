#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Soft Reboot MVP test."""

import logging
from typing import cast

from honeydew.interfaces.device_classes.fuchsia_device import FuchsiaDevice
from honeydew.mobly_controller import fuchsia_device
from mobly import base_test, test_runner

_LOGGER = logging.getLogger(__name__)


# pylint: disable=attribute-defined-outside-init
class SoftRebootTest(base_test.BaseTestClass):
    """Soft Reboot MVP test"""

    def setup_generated_tests(self):
        test_arg_tuple_list = []

        for iteration in range(1, int(self.user_params["num_reboots"]) + 1):
            test_arg_tuple_list.append((iteration,))

        self.generate_tests(
            test_logic=self._test_logic,
            name_func=self._name_func,
            arg_sets=test_arg_tuple_list)

    def setup_class(self):
        fuchsia_devices = self.register_controller(fuchsia_device)
        self.dut = cast(FuchsiaDevice, fuchsia_devices[0])

    def on_fail(self, record):
        if not hasattr(self, "dut"):
            return

        try:
            self.dut.snapshot(
                directory=self.current_test_info.output_path,
                snapshot_file=f"{self.dut.name}.zip")
        except Exception:  # pylint: disable=broad-except
            _LOGGER.warning("Unable to take snapshot")

    def _test_logic(self, iteration: int):
        _LOGGER.info("Starting the Soft Reboot test iteration# %s", iteration)
        self.dut.reboot()
        _LOGGER.info(
            "Successfully ended the Soft Reboot test iteration# %s", iteration)

    def _name_func(self, iteration: int) -> str:
        return f"test_soft_reboot_{iteration}"


if __name__ == '__main__':
    test_runner.main()
