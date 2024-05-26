# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Real time clock (RTC) affordance using the FuchsiaController."""

import asyncio
import datetime

import fidl.fuchsia_hardware_rtc as frtc
import fuchsia_controller_py

from honeydew import errors
from honeydew.interfaces.affordances import rtc
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import (
    fuchsia_controller as fuchsia_controller_lib,
)
from honeydew.typing import custom_types

CAPABILITY = "fuchsia.hardware.rtc.Service/default/device"


class Rtc(rtc.Rtc):
    """Affordance for the fuchsia.hardware.rtc.Device protocol."""

    # TODO(b/316959472) Use toolbox once RTC service lands in the toolbox realm.
    #
    # Service moniker for the NXP PCF8563 chip on a Khadas vim3 board.
    # Currently, this is board-specific. Once the RTC protocol lands and is
    # routable from the toolbox realm, this affordance can be made
    # board-agnostic.

    # TODO(b/340607972): To allow for smooth transition from vim3 to vim3-devicetree, both monikers
    # will be tried. Whichever path exists will be used. Once the migration is complete, the old
    # moniker can be discarded.
    MONIKER_OLD = "/bootstrap/pkg-drivers:dev.sys.platform.05_00_2.i2c-0.aml-i2c.i2c.i2c-0-81"
    MONIKER_NEW = "/bootstrap/pkg-drivers:dev.sys.platform.i2c-5000.i2c-5000_group.aml-i2c.i2c.i2c-0-81"

    def __init__(
        self,
        fuchsia_controller: fuchsia_controller_lib.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
    ) -> None:
        """Initializer."""
        self._controller = fuchsia_controller

        # This needs to be called once upon __init__(), and any time the device
        # is rebooted. On reboot, the connection is lost and needs to be
        # re-established.
        self._connect_proxy()
        reboot_affordance.register_for_on_device_boot(self._connect_proxy)

    def _connect_proxy(self) -> None:
        """Connect the RTC Device protocol proxy."""
        ep_old = custom_types.FidlEndpoint(
            self.__class__.MONIKER_OLD, CAPABILITY
        )
        ep_new = custom_types.FidlEndpoint(
            self.__class__.MONIKER_NEW, CAPABILITY
        )
        try:
            self._proxy: frtc.Device.Client = frtc.Device.Client(
                self._controller.connect_device_proxy(ep_old)
            )
        except RuntimeError:
            # Try connecting through the other moniker.
            try:
                self._proxy = frtc.Device.Client(
                    self._controller.connect_device_proxy(ep_new)
                )
            except RuntimeError:
                raise errors.HoneydewRtcError(
                    "Failed to connect to either moniker."
                ) from None

    # Protocol methods.
    def get(self) -> datetime.datetime:
        """See base class."""
        try:
            time = asyncio.run(self._proxy.get())
        except fuchsia_controller_py.ZxStatus as status:
            msg = f"Device.Get() error {status}"
            raise errors.HoneydewRtcError(msg) from status

        response = time.response.rtc
        return datetime.datetime(
            response.year,
            response.month,
            response.day,
            response.hours,
            response.minutes,
            response.seconds,
        )

    def set(self, time: datetime.datetime) -> None:
        """See base class."""
        ftime = frtc.Time(
            time.second, time.minute, time.hour, time.day, time.month, time.year
        )

        try:
            result = asyncio.run(self._proxy.set(rtc=ftime))
        except fuchsia_controller_py.ZxStatus as status:
            msg = f"Device.Set() error {status}"
            raise errors.HoneydewRtcError(msg) from status

        if result.response.status != fuchsia_controller_py.ZxStatus.ZX_OK:
            msg = f"Device.Set() error {result.response.status}"
            raise errors.HoneydewRtcError(msg)
