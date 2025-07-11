# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import logging

from fuchsia_base_test import fuchsia_base_test
from honeydew.fuchsia_device import fuchsia_device
from mobly import asserts, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class RebootReasonTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_class(self) -> None:
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_reboot_reason(self) -> None:
        _LOGGER.info("[test_reboot_reason] Rebooting device...")
        # Under the hood, this makes a FIDL call over
        # fuchsia.hardware.power.statecontrol/Admin::PerformReboot() with reboot reason
        # USER_REQUEST.
        self.dut.reboot()
        _LOGGER.info("[test_reboot_reason] Device has rebooted successfully")
        asserts.assert_equal(self.dut.last_reboot_reason, "USER_REQUEST")


if __name__ == "__main__":
    test_runner.main()
