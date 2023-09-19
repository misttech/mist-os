#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.wlan_policy.py."""

import unittest

from honeydew.affordances.fuchsia_controller.wlan import \
    wlan_policy as fc_wlan_policy
from honeydew.typing.wlan import SecurityType


# pylint: disable=protected-access
class ComponentFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.wlan_policy.py."""

    def setUp(self) -> None:
        super().setUp()

        self.wlan_policy_obj = fc_wlan_policy.WlanPolicy()

        self.assertIsInstance(self.wlan_policy_obj, fc_wlan_policy.WlanPolicy)

    def test_create_client_controller(self) -> None:
        """Test for WlanPolicy.create_client_controller() method."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.create_client_controller()

    def test_get_saved_networks(self) -> None:
        """Test for WlanPolicy.get_saved_networks()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.get_saved_networks()

    def test_get_update(self) -> None:
        """Test for WlanPolicy.get_update()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.get_update()

    def test_remove_all_networks(self) -> None:
        """Test for WlanPolicy.remove_all_networks()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.remove_all_networks()

    def test_save_network(self) -> None:
        """Test for WlanPolicy.save_network()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.save_network(
                target_ssid="test", security_type=SecurityType.NONE)

    def test_set_new_update_listener(self) -> None:
        """Test for WlanPolicy.set_new_update_listener()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.set_new_update_listener()

    def test_start_client_connections(self) -> None:
        """Test for WlanPolicy.start_client_connections()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.start_client_connections()

    def test_stop_client_connections(self) -> None:
        """Test for WlanPolicy.stop_client_connections()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.stop_client_connections()


if __name__ == "__main__":
    unittest.main()
