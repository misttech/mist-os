# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for FFX transport."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)

_REBOOT: list[str] = ["target", "reboot"]


class FFXWaitForRCSDisconnectionTests(fuchsia_base_test.FuchsiaBaseTest):
    """Test class to test FFX.wait_for_rcs_disconnection().

    This is included in a separate test class as it involves rebooting the
    device.
    """

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_wait_for_rcs_connection(self) -> None:
        """Test case for FFX.wait_for_rcs_connection()."""
        self.device.ffx.wait_for_rcs_connection()

        self.device.ffx.run(_REBOOT)

        self.device.ffx.wait_for_rcs_disconnection()

        self.device.ffx.wait_for_rcs_connection()

        # Make the device ready after reboot
        self.device.on_device_boot()


if __name__ == "__main__":
    test_runner.main()
