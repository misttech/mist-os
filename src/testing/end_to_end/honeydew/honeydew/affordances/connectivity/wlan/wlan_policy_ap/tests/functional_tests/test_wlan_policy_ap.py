# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for WLAN policy access point affordance."""

import random
import string
import time

from mobly import asserts, test_runner
from wlan_base_test import wlan_base_test

from honeydew.affordances.connectivity.netstack.types import (
    InterfaceProperties,
    PortClass,
)
from honeydew.affordances.connectivity.wlan.utils.types import (
    AccessPointState,
    ConnectedClientInformation,
    ConnectivityMode,
    NetworkIdentifier,
    OperatingBand,
    OperatingState,
    SecurityType,
)
from honeydew.interfaces.device_classes import fuchsia_device

# Time to wait for a WLAN interface to become available.
WLAN_INTERFACE_TIMEOUT = 30


def random_str(
    size: int = 6, chars: str = string.ascii_lowercase + string.digits
) -> str:
    """Generate a random string.

    Args:
        size: Length of output string
        chars: Characters to use

    Returns:
        A random string of length size using the characters in chars.
    """
    return "".join(random.choice(chars) for _ in range(size))


class WlanPolicyApTests(wlan_base_test.WlanBaseTest):
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

    def teardown_test(self) -> None:
        # Don't allow access points to leak into other tests.
        self.device.wlan_policy_ap.stop_all()
        return super().teardown_test()

    def test_ap_methods(self) -> None:
        """Verify WLAN policy access point methods."""
        self.device.wlan_policy_ap.stop_all()
        self.device.wlan_policy_ap.set_new_update_listener()
        asserts.assert_equal(
            self.device.wlan_policy_ap.get_update(),
            [],
        )

        test_ssid = random_str()
        self.device.wlan_policy_ap.start(
            test_ssid,
            SecurityType.NONE,
            None,
            ConnectivityMode.LOCAL_ONLY,
            OperatingBand.ONLY_2_4GHZ,
        )
        asserts.assert_equal(
            self.device.wlan_policy_ap.get_update(),
            [
                AccessPointState(
                    state=OperatingState.STARTING,
                    mode=ConnectivityMode.LOCAL_ONLY,
                    band=OperatingBand.ONLY_2_4GHZ,
                    frequency=None,
                    clients=None,
                    id=NetworkIdentifier(
                        ssid=test_ssid, security_type=SecurityType.NONE
                    ),
                )
            ],
        )
        asserts.assert_equal(
            self.device.wlan_policy_ap.get_update(),
            [
                AccessPointState(
                    state=OperatingState.ACTIVE,
                    mode=ConnectivityMode.LOCAL_ONLY,
                    band=OperatingBand.ONLY_2_4GHZ,
                    frequency=None,
                    clients=None,
                    id=NetworkIdentifier(
                        ssid=test_ssid, security_type=SecurityType.NONE
                    ),
                )
            ],
        )
        got_states = self.device.wlan_policy_ap.get_update()
        asserts.assert_greater_equal(got_states[0].frequency, 2412)  # channel 1
        asserts.assert_less_equal(got_states[0].frequency, 2484)  # channel 14
        asserts.assert_equal(
            got_states,
            [
                AccessPointState(
                    state=OperatingState.ACTIVE,
                    mode=ConnectivityMode.LOCAL_ONLY,
                    band=OperatingBand.ONLY_2_4GHZ,
                    frequency=got_states[0].frequency,
                    clients=ConnectedClientInformation(count=0),
                    id=NetworkIdentifier(
                        ssid=test_ssid, security_type=SecurityType.NONE
                    ),
                )
            ],
        )

        self.device.wlan_policy_ap.set_new_update_listener()
        got_states = self.device.wlan_policy_ap.get_update()
        asserts.assert_is_not_none(got_states[0].frequency)
        asserts.assert_equal(
            got_states,
            [
                AccessPointState(
                    state=OperatingState.ACTIVE,
                    mode=ConnectivityMode.LOCAL_ONLY,
                    band=OperatingBand.ONLY_2_4GHZ,
                    frequency=got_states[0].frequency,
                    clients=ConnectedClientInformation(count=0),
                    id=NetworkIdentifier(
                        ssid=test_ssid, security_type=SecurityType.NONE
                    ),
                )
            ],
        )

        self.device.wlan_policy_ap.stop(test_ssid, SecurityType.NONE, None)
        asserts.assert_equal(
            self.device.wlan_policy_ap.get_update(),
            [],
        )


if __name__ == "__main__":
    test_runner.main()
