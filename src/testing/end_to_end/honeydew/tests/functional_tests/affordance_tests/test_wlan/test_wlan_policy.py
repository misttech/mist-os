#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for wlan policy affordance."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts
from mobly import test_runner

from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing.wlan import NetworkConfig
from honeydew.typing.wlan import SecurityType

_LOGGER: logging.Logger = logging.getLogger(__name__)


# TODO(b/299192230): Add test_stop_client_connections
class WlanPolicyTests(fuchsia_base_test.FuchsiaBaseTest):
    """WlanPolicy affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_get_saved_networks(self) -> None:
        """Test case for wlan_policy.get_saved_networks().

        This test clears all networks then saves 2 new networks and asserts they
        were saved successfully.

        This test case calls the following wlan_policy methods:
            * wlan_policy.create_client_controller()
            * wlan_policy.remove_all_networks()
            * wlan_policy.save_network()
            * wlan_policy.get_saved_networks()
        """
        if self._is_fuchsia_controller_based_device(self.device):
            with asserts.assert_raises(NotImplementedError):
                self.device.wlan_policy.create_client_controller()
                self.device.wlan_policy.remove_all_networks()
                self.device.wlan_policy.save_network("test", SecurityType.NONE)
                self.device.wlan_policy.get_saved_networks()
            return

        self.device.wlan_policy.create_client_controller()
        self.device.wlan_policy.remove_all_networks()
        networks: list[
            NetworkConfig] = self.device.wlan_policy.get_saved_networks()

        asserts.assert_equal(len(networks), 0)

        self.device.wlan_policy.save_network("a", SecurityType.NONE)
        self.device.wlan_policy.save_network("b", SecurityType.WPA, "12345678")

        expected_networks: list[NetworkConfig] = [
            NetworkConfig(
                ssid="a",
                security_type=SecurityType.NONE,
                credential_type="None",
                credential_value=""),
            NetworkConfig(
                ssid="b",
                security_type=SecurityType.WPA,
                credential_type="Password",
                credential_value="12345678")
        ]

        networks = self.device.wlan_policy.get_saved_networks()

        asserts.assert_equal(sorted(networks), sorted(expected_networks))


if __name__ == "__main__":
    test_runner.main()
