# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.


import logging
import typing

import fidl.fuchsia_hardware_power_statecontrol as power_statecontrol
from fuchsia_controller_py import ZxStatus
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod
from mobly import base_test, test_runner
from mobly_controller import fuchsia_device

_LOGGER = logging.getLogger("reboot_test")


class FuchsiaControllerTests(AsyncAdapter, base_test.BaseTestClass):
    def setup_class(self) -> None:
        self.fuchsia_devices: typing.List[
            fuchsia_device.FuchsiaDevice
        ] = self.register_controller(fuchsia_device)
        self.device = self.fuchsia_devices[0]
        self.device.set_ctx(self)

    @asyncmethod
    async def test_fuchsia_device_reboot(self) -> None:
        """Attempts to reboot a device."""
        if self.device.ctx is None:
            raise ValueError(f"Device: {self.device.target} has no context")
        # [START reboot_example]
        ch = self.device.ctx.connect_device_proxy(
            "bootstrap/shutdown_shim", power_statecontrol.AdminMarker
        )
        admin = power_statecontrol.AdminClient(ch)
        # Makes a coroutine to ensure that a PEER_CLOSED isn't received from attempting
        # to write to the channel.
        coro = admin.reboot(reason=power_statecontrol.RebootReason.USER_REQUEST)
        try:
            _LOGGER.info("Issuing reboot command")
            await coro
        except ZxStatus as status:
            if status.raw() != ZxStatus.ZX_ERR_PEER_CLOSED:
                raise status
            _LOGGER.info("Device reboot command sent")
        # [END reboot_example]
        await self.device.wait_offline()
        await self.device.wait_online()


if __name__ == "__main__":
    test_runner.main()
