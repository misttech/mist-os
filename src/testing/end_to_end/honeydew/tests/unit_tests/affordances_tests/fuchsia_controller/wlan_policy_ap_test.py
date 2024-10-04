# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.wlan.wlan_policy_ap."""

import unittest
from unittest import mock

from honeydew.affordances.fuchsia_controller.wlan import wlan_policy_ap
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport
from honeydew.typing.wlan import ConnectivityMode, OperatingBand, SecurityType

_TEST_SSID = "ThepromisedLAN"


# pylint: disable=protected-access
class WlanPolicyApFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.wlan.wlan_policy_ap."""

    def setUp(self) -> None:
        super().setUp()

        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice,
            autospec=True,
        )
        self.fuchsia_device_close_obj = mock.MagicMock(
            spec=affordances_capable.FuchsiaDeviceClose,
            autospec=True,
        )
        self.fc_transport_obj = mock.MagicMock(
            spec=fc_transport.FuchsiaController,
            autospec=True,
        )
        self.ffx_transport_obj = mock.MagicMock(
            spec=ffx_transport.FFX,
            autospec=True,
        )

        self.ffx_transport_obj.run.return_value = "".join(
            wlan_policy_ap._REQUIRED_CAPABILITIES
        )

        self.wlan_policy_ap_obj = wlan_policy_ap.WlanPolicyAp(
            device_name="fuchsia-emulator",
            ffx=self.ffx_transport_obj,
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
            fuchsia_device_close=self.fuchsia_device_close_obj,
        )

        self.assertFalse(
            self.wlan_policy_ap_obj._access_point_controller.access_point_state_updates_server_task.cancelled(),
            "Expected access point state update server to be running",
        )

    def tearDown(self) -> None:
        self.wlan_policy_ap_obj._close()
        return super().tearDown()

    def test_verify_supported(self) -> None:
        """Verify _verify_supported fails."""
        self.ffx_transport_obj.run.return_value = ""

        with self.assertRaises(NotSupportedError):
            self.wlan_policy_ap_obj = wlan_policy_ap.WlanPolicyAp(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
                fuchsia_device_close=self.fuchsia_device_close_obj,
            )

    def test_init_register_for_on_device_boot(self) -> None:
        """Verify WlanPolicyAp registers on_device_boot."""
        self.reboot_affordance_obj.register_for_on_device_boot.assert_called_once_with(
            self.wlan_policy_ap_obj._connect_proxy
        )

    def test_init_connect_proxy(self) -> None:
        """Verify WlanPolicyAp connects to
        fuchsia.wlan.policy/AccessPointProvider and AccessPointListener."""
        self.assertIsNotNone(
            self.wlan_policy_ap_obj._access_point_provider_proxy
        )
        self.assertIsNotNone(
            self.wlan_policy_ap_obj._access_point_listener_proxy
        )

    def test_start(self) -> None:
        """Verify WlanPolicyAp.start()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_ap_obj.start(
                _TEST_SSID,
                SecurityType.NONE,
                None,
                ConnectivityMode.LOCAL_ONLY,
                OperatingBand.ANY,
            )

    def test_stop(self) -> None:
        """Verify WlanPolicyAp.stop()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_ap_obj.stop(_TEST_SSID, SecurityType.NONE, None)

    def test_stop_all(self) -> None:
        """Verify WlanPolicyAp.stop_all()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_ap_obj.stop_all()

    def test_set_new_update_listener(self) -> None:
        """Verify WlanPolicyAp.set_new_update_listener()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_ap_obj.set_new_update_listener()

    def test_get_update(self) -> None:
        """Verify WlanPolicyAp.get_update()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_ap_obj.get_update()


if __name__ == "__main__":
    unittest.main()
