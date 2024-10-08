# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for WLAN policy access point affordance."""

import time

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing.netstack import InterfaceProperties, PortClass

# Time to wait for a WLAN interface to become available.
WLAN_INTERFACE_TIMEOUT = 30


class WlanPolicyApTests(fuchsia_base_test.FuchsiaBaseTest):
    """WlanPolicyAp affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

        # Wait for a WLAN interface to become available.
        interfaces: list[InterfaceProperties] = []
        end_time = time.time() + WLAN_INTERFACE_TIMEOUT
        while time.time() < end_time:
            interfaces = self.device.netstack.list_interfaces()
            for interface in interfaces:
                if interface.port_class is PortClass.WLAN_CLIENT:
                    return
            time.sleep(1)  # Prevent denial-of-service
        asserts.abort_class(
            f"Expected presence of a WLAN interface, got {interfaces}"
        )

    def test_ap_methods(self) -> None:
        """Verify WLAN policy access point methods."""
        self.device.wlan_policy_ap.stop_all()
        self.device.wlan_policy_ap.set_new_update_listener()
        asserts.assert_equal(
            self.device.wlan_policy_ap.get_update(),
            [],
        )


if __name__ == "__main__":
    test_runner.main()
