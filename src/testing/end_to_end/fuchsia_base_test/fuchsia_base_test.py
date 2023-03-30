#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia base test class."""

import logging
from typing import List

from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.mobly_controller import \
    fuchsia_device as fuchsia_device_mobly_controller
from mobly import base_test, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaBaseTest(base_test.BaseTestClass):
    """Fuchsia base test class.

    Attributes:
        fuchsia_devices: List of FuchsiaDevice objects.
        test_case_path: Directory pointing to a specific test case artifacts.
    """

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Instantiates all fuchsia devices into self.fuchsia_devices
        """
        self.fuchsia_devices: List[
            fuchsia_device.FuchsiaDevice] = self.register_controller(
                fuchsia_device_mobly_controller)

    def setup_test(self) -> None:
        """setup_test is called once before running each test.

        It does the following things:
            * Stores the current test case path into self.test_case_path
        """
        self.test_case_path: str = \
            f"{self.log_path}/{self.current_test_info.name}"

    def teardown_test(self) -> None:
        """teardown_test is called once after running each test.

        It does the following things:
            * Takes snapshot of all the fuchsia devices and stores it under
              test case directory
        """
        for fx_device in self.fuchsia_devices:
            try:
                fx_device.snapshot(directory=self.test_case_path)
            except Exception:  # pylint: disable=broad-except
                _LOGGER.warning("Unable to take snapshot of %s", fx_device.name)


if __name__ == "__main__":
    test_runner.main()
