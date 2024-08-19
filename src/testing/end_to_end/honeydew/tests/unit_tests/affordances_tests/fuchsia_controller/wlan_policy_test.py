# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.wlan.wlan_policy."""

import unittest
from unittest import mock

from honeydew.affordances.fuchsia_controller.wlan import wlan_policy
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport
from honeydew.typing.wlan import SecurityType


# pylint: disable=protected-access
class WlanPolicyFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.wlan.wlan_policy."""

    def setUp(self) -> None:
        super().setUp()

        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice,
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
            wlan_policy._REQUIRED_CAPABILITIES
        )

        self.wlan_policy_obj = wlan_policy.WlanPolicy(
            device_name="fuchsia-emulator",
            ffx=self.ffx_transport_obj,
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )

    def test_verify_supported(self) -> None:
        """Test if _verify_supported works."""
        self.ffx_transport_obj.run.return_value = ""
        with self.assertRaises(NotSupportedError):
            self.wlan_policy_obj = wlan_policy.WlanPolicy(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
            )

    def test_connect(self) -> None:
        """Test if connect works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.connect("", SecurityType.NONE)

    def test_create_client_controller(self) -> None:
        """Test if create_client_controller works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.create_client_controller()

    def test_get_saved_networks(self) -> None:
        """Test if get_saved_networks works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.get_saved_networks()

    def test_get_update(self) -> None:
        """Test if get_update works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.get_update()

    def test_remove_all_networks(self) -> None:
        """Test if remove_all_networks works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.remove_all_networks()

    def test_remove_network(self) -> None:
        """Test if remove_network works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.remove_network("", SecurityType.NONE)

    def test_save_network(self) -> None:
        """Test if save_network works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.save_network("", SecurityType.NONE)

    def test_scan_for_networks(self) -> None:
        """Test if scan_for_networks works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.scan_for_networks()

    def test_set_new_update_listener(self) -> None:
        """Test if set_new_update_listener works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.set_new_update_listener()

    def test_start_client_connections(self) -> None:
        """Test if start_client_connections works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.start_client_connections()

    def test_stop_client_connections(self) -> None:
        """Test if stop_client_connections works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.stop_client_connections()


if __name__ == "__main__":
    unittest.main()
