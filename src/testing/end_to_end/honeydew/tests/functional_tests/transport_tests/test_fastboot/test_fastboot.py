#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Fastboot transport."""

import logging
from typing import List

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts
from mobly import test_runner

from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.interfaces.device_classes import transports_capable

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FastbootTransportTests(fuchsia_base_test.FuchsiaBaseTest):
    """Fastboot transport tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
            * Calls some Fastboot transport method to initialize Fastboot
              transport (as it may involve device reboots)
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        assert isinstance(self.device, transports_capable.FastbootCapableDevice)

        # Calling some fastboot method such that Fastboot __init__ gets called
        # which will retrieve the fastboot node-id.
        # Reason for doing this is, if DUT uses TCP based fastboot connection
        # then retrieving the fastboot node-id involves :
        #   * rebooting the device into fastboot mode,
        #   * retrieve the fastboot node-id
        #   * reboot back to fuchsia mode
        # So to avoid all these steps in actual test case, we are calling some
        # fastboot method here in setup_class
        self._fastboot_node_id: str = self.device.fastboot.node_id

    def teardown_test(self) -> None:
        """teardown_test is called once after running each test.

        It does the following things:
            * Ensures device is in fuchsia mode.
        """
        super().teardown_test()
        assert isinstance(self.device, transports_capable.FastbootCapableDevice)
        if self.device.fastboot.is_in_fastboot_mode():
            _LOGGER.warning("%s is in fastboot mode which is not expected. "\
                            "Rebooting to fuchsia mode",
                            self.device.device_name)
            self.device.fastboot.boot_to_fuchsia_mode()

    def test_fastboot_node_id(self) -> None:
        """Test case for Fastboot.node_id."""
        assert isinstance(self.device, transports_capable.FastbootCapableDevice)
        # Note - If "node_id" is specified in "expected_values" in
        # params.yml then compare with it.
        if self.user_params["expected_values"] and self.user_params[
                "expected_values"].get("node_id"):
            asserts.assert_equal(
                self._fastboot_node_id,
                self.user_params["expected_values"]["node_id"])
        else:
            asserts.assert_is_not_none(self._fastboot_node_id)
            asserts.assert_is_instance(self._fastboot_node_id, str)

    def test_fastboot_methods(self) -> None:
        """Test case that puts the device in fastboot mode, runs a command in
        fastboot mode and reboots the device back to fuchsia mode."""
        assert isinstance(self.device, transports_capable.FastbootCapableDevice)

        self.device.fastboot.boot_to_fastboot_mode()

        asserts.assert_false(
            self.device.fastboot.is_in_fuchsia_mode(),
            msg=f"{self.device.device_name} is in fuchsia mode when not " \
                f"expected"
        )
        asserts.assert_true(
            self.device.fastboot.is_in_fastboot_mode(),
            msg=f"{self.device.device_name} is not in fastboot mode which " \
                f"is not expected"
        )

        cmd: List[str] = ["getvar", "hw-revision"]
        self.device.fastboot.run(cmd)

        self.device.fastboot.boot_to_fuchsia_mode()

        asserts.assert_true(
            self.device.fastboot.is_in_fuchsia_mode(),
            msg=f"{self.device.device_name} is not in fuchsia mode which is " \
                f"not expected"
        )
        asserts.assert_false(
            self.device.fastboot.is_in_fastboot_mode(),
            msg=
            f"{self.device.device_name} is in fastboot mode when not expected")


if __name__ == "__main__":
    test_runner.main()
