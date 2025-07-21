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

        # We always expect "USER_REQUEST", but now that we have set up the test, we are finding
        # instances of true bugs where something goes wrong during shutdown. To help with
        # https://fxbug.dev/432864757, we want a different message on failure depending on what
        # the reason is to better inform whoemever is looking at the test failure.
        if self.dut.last_reboot_reason == "ROOT_JOB_TERMINATION":
            asserts.assert_equal(
                self.dut.last_reboot_reason,
                "USER_REQUEST",
                msg=(
                    "There was likely a driver hang during userspace shutdown. See"
                    " serial_log.txt and https://fxbug.dev/432968401"
                ),
            )
        elif self.dut.last_reboot_reason in ["COLD", "BRIEF_POWER_LOSS"]:
            asserts.assert_equal(
                self.dut.last_reboot_reason,
                "USER_REQUEST",
                msg=(
                    "There was likely a hardware reboot due to a driver action during"
                    " userspace shutdown. See serial_log.txt and"
                    " https://fxbug.dev/433253369"
                ),
            )
        else:
            asserts.assert_equal(self.dut.last_reboot_reason, "USER_REQUEST")


if __name__ == "__main__":
    test_runner.main()
