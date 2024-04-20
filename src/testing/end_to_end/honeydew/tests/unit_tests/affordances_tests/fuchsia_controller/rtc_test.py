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

from honeydew import errors
from honeydew.affordances.fuchsia_controller import rtc
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import fuchsia_controller

# Alias for convenience.
ZX_OK = fuchsia_controller_py.ZxStatus.ZX_OK
ZX_ERR_INTERNAL = fuchsia_controller_py.ZxStatus.ZX_ERR_INTERNAL


class RtcTest(unittest.TestCase):
    def setUp(self):
        self.m_run = self.enterContext(mock.patch.object(asyncio, "run"))
        self.m_proxy = self.enterContext(
            mock.patch.object(frtc.Device, "Client")
        ).return_value

        transport = mock.create_autospec(fuchsia_controller.FuchsiaController)
        reboot_af = mock.create_autospec(
            affordances_capable.RebootCapableDevice
        )

        self.rtc = rtc.Rtc(transport, reboot_af)
        transport.connect_device_proxy.assert_called_once()
        reboot_af.register_for_on_device_boot.assert_called_once()

    def test_rtc_get(self):
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

    def test_rtc_get_exception(self):
        self.m_run.side_effect = fuchsia_controller_py.ZxStatus

        msg = r"Device\.Get\(\) error"
        with self.assertRaisesRegex(errors.HoneydewRtcError, msg):
            self.rtc.get()

        self.m_proxy.get.assert_called_once()
        self.m_run.assert_called_once()

    def test_rtc_set(self):
        time = datetime.datetime(2022, 2, 5, 15, 50, 23)
        self.m_run.return_value.response.status = ZX_OK

        want = frtc.Time(
            time.second, time.minute, time.hour, time.day, time.month, time.year
        )

        self.rtc.set(time)
        self.m_proxy.set.assert_called_once_with(rtc=want)
        self.m_run.assert_called_once()

    def test_rtc_set_error(self):
        """Test errors returned by Set()

        Unlike Get, the Set API does not currently use `-> () error zx.Status`
        syntax. It can return errors in one of two ways, either by a failed FIDL
        transaction, or by successfully returning an error in the struct.

        This tests the struct case.
        """
        time = datetime.datetime(2022, 2, 5, 15, 50, 23)
        self.m_run.return_value.response.status = ZX_ERR_INTERNAL

        msg = r"Device\.Set\(\) error"
        with self.assertRaisesRegex(errors.HoneydewRtcError, msg):
            self.rtc.set(time)

        self.m_proxy.set.assert_called_once()
        self.m_run.assert_called_once()

    def test_rtc_set_exception(self):
        """Test errors returned by Set()

        Unlike Get, the Set API does not currently use `-> () error zx.Status`
        syntax. It can return errors in one of two ways, either by a failed FIDL
        transaction, or by successfully returning an error in the struct.

        This tests the FIDL-failure case.
        """
        time = datetime.datetime(2022, 2, 5, 15, 50, 23)
        self.m_run.side_effect = fuchsia_controller_py.ZxStatus

        msg = r"Device\.Set\(\) error"
        with self.assertRaisesRegex(errors.HoneydewRtcError, msg):
            self.rtc.set(time)

        self.m_proxy.set.assert_called_once()
        self.m_run.assert_called_once()


if __name__ == "__main__":
    unittest.main()
