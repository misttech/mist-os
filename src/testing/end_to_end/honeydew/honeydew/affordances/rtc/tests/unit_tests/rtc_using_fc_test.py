# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.rtc.py."""

import asyncio
import datetime
import unittest
from unittest import mock

import fidl.fuchsia_hardware_rtc as frtc
import fuchsia_controller_py

from honeydew.affordances.rtc import rtc_using_fc
from honeydew.affordances.rtc.errors import HoneydewRtcError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import fuchsia_controller

# Alias for convenience.
ZX_OK = fuchsia_controller_py.ZxStatus.ZX_OK
ZX_ERR_INTERNAL = fuchsia_controller_py.ZxStatus.ZX_ERR_INTERNAL


class RtcFcTests(unittest.TestCase):
    def setUp(self) -> None:
        self.m_run = self.enterContext(mock.patch.object(asyncio, "run"))
        self.m_proxy = self.enterContext(
            mock.patch.object(frtc.Device, "Client")
        ).return_value

        self.transport = mock.create_autospec(
            fuchsia_controller.FuchsiaController
        )
        self.reboot_af = mock.create_autospec(
            affordances_capable.RebootCapableDevice
        )

        self.rtc = rtc_using_fc.RtcUisngFc(self.transport, self.reboot_af)
        self.transport.connect_device_proxy.assert_called_once()
        self.reboot_af.register_for_on_device_boot.assert_called_once()

    def test_rtc_setup_fallback(self) -> None:
        self.transport.reset_mock()
        self.reboot_af.reset_mock()

        self.transport.connect_device_proxy.side_effect = [
            RuntimeError("Device not found"),
            ZX_OK,
        ]

        _ = rtc_using_fc.RtcUisngFc(self.transport, self.reboot_af)
        self.assertEqual(self.transport.connect_device_proxy.call_count, 2)
        self.reboot_af.register_for_on_device_boot.assert_called_once()

        (ep1,), _ = self.transport.connect_device_proxy.call_args_list[0]
        (ep2,), _ = self.transport.connect_device_proxy.call_args_list[1]

        self.assertEqual(rtc_using_fc.RtcUisngFc.MONIKER_OLD, ep1.moniker)
        self.assertEqual(rtc_using_fc.CAPABILITY, ep1.protocol)

        self.assertEqual(rtc_using_fc.RtcUisngFc.MONIKER_NEW, ep2.moniker)
        self.assertEqual(rtc_using_fc.CAPABILITY, ep2.protocol)

    def test_rtc_get(self) -> None:
        chip_time = frtc.Time(23, 50, 15, 5, 2, 2022)
        self.m_run.return_value.response.rtc = chip_time

        want = datetime.datetime(
            chip_time.year,
            chip_time.month,
            chip_time.day,
            chip_time.hours,
            chip_time.minutes,
            chip_time.seconds,
        )

        self.assertEqual(want, self.rtc.get())
        self.m_proxy.get.assert_called_once()
        self.m_run.assert_called_once()

    def test_rtc_get_exception(self) -> None:
        self.m_run.side_effect = fuchsia_controller_py.ZxStatus

        msg = r"Device\.Get\(\) error"
        with self.assertRaisesRegex(HoneydewRtcError, msg):
            self.rtc.get()

        self.m_proxy.get.assert_called_once()
        self.m_run.assert_called_once()

    def test_rtc_set(self) -> None:
        time = datetime.datetime(2022, 2, 5, 15, 50, 23)
        self.m_run.return_value.response.status = ZX_OK

        want = frtc.Time(
            time.second, time.minute, time.hour, time.day, time.month, time.year
        )

        self.rtc.set(time)
        self.m_proxy.set.assert_called_once_with(rtc=want)
        self.m_run.assert_called_once()

    def test_rtc_set_error(self) -> None:
        """Test errors returned by Set()

        Unlike Get, the Set API does not currently use `-> () error zx.Status`
        syntax. It can return errors in one of two ways, either by a failed FIDL
        transaction, or by successfully returning an error in the struct.

        This tests the struct case.
        """
        time = datetime.datetime(2022, 2, 5, 15, 50, 23)
        self.m_run.return_value.response.status = ZX_ERR_INTERNAL

        msg = r"Device\.Set\(\) error"
        with self.assertRaisesRegex(HoneydewRtcError, msg):
            self.rtc.set(time)

        self.m_proxy.set.assert_called_once()
        self.m_run.assert_called_once()

    def test_rtc_set_exception(self) -> None:
        """Test errors returned by Set()

        Unlike Get, the Set API does not currently use `-> () error zx.Status`
        syntax. It can return errors in one of two ways, either by a failed FIDL
        transaction, or by successfully returning an error in the struct.

        This tests the FIDL-failure case.
        """
        time = datetime.datetime(2022, 2, 5, 15, 50, 23)
        self.m_run.side_effect = fuchsia_controller_py.ZxStatus

        msg = r"Device\.Set\(\) error"
        with self.assertRaisesRegex(HoneydewRtcError, msg):
            self.rtc.set(time)

        self.m_proxy.set.assert_called_once()
        self.m_run.assert_called_once()


if __name__ == "__main__":
    unittest.main()
