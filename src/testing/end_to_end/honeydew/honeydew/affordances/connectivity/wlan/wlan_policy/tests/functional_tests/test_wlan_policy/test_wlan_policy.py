# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for wlan policy affordance."""

import random
import string
import time
from collections.abc import Iterator

from antlion.controllers import access_point
from antlion.controllers.ap_lib import hostapd_constants
from mobly import asserts, signals, test_runner
from wlan_base_test import wlan_base_test

from honeydew.affordances.connectivity.wlan.utils.types import (
    ClientStateSummary,
    ConnectionState,
    DisconnectStatus,
    NetworkConfig,
    NetworkIdentifier,
    NetworkState,
    RequestStatus,
    SecurityType,
    WlanClientState,
)
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing.netstack import PortClass

# Time to wait for a WLAN interface to become available.
WLAN_INTERFACE_TIMEOUT = 30
# Time to wait for a WLAN client state update.
DEFAULT_GET_UPDATE_TIMEOUT = 60


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


class WlanPolicyTests(wlan_base_test.WlanBaseTest):
    """WlanPolicy affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self.device.wlan_policy.create_client_controller()

        access_points: list[
            access_point.AccessPoint
        ] | None = self.register_controller(
            access_point, required=False, min_number=0
        )

        self.access_point: access_point.AccessPoint | None = (
            access_points[0] if access_points else None
        )

        self.wait_for_interface(self.device.netstack, PortClass.WLAN_CLIENT)

    def setup_test(self) -> None:
        super().setup_test()
        self.device.wlan_policy.remove_all_networks()

    def teardown_test(self) -> None:
        if self.access_point is not None:
            self.access_point.close()
        super().teardown_test()

    def test_client_methods(self) -> None:
        """Test case for wlan_policy client methods.

        This test starts and stops client connections and checks that they are in the
        expected states.
        """
        self.device.wlan_policy.start_client_connections()
        self.device.wlan_policy.set_new_update_listener()
        asserts.assert_equal(
            self.device.wlan_policy.get_update(),
            ClientStateSummary(
                state=WlanClientState.CONNECTIONS_ENABLED,
                networks=[],
            ),
        )

        self.device.wlan_policy.stop_client_connections()
        asserts.assert_equal(
            self.device.wlan_policy.get_update(),
            ClientStateSummary(
                state=WlanClientState.CONNECTIONS_DISABLED,
                networks=[],
            ),
        )

        # Verify connections are still disabled after resetting the update
        # listener.
        self.device.wlan_policy.set_new_update_listener()
        asserts.assert_equal(
            self.device.wlan_policy.get_update(),
            ClientStateSummary(
                state=WlanClientState.CONNECTIONS_DISABLED,
                networks=[],
            ),
        )

    def test_ap_auto_connect(self) -> None:
        """Verify Fuchsia can auto-connect to a saved network."""
        if not self.access_point:
            raise signals.TestSkip("Access point required for this test")

        test_ssid = random_str()
        access_point.setup_ap(
            access_point=self.access_point,
            profile_name="whirlwind",
            channel=hostapd_constants.AP_DEFAULT_CHANNEL_2G,
            ssid=test_ssid,
        )

        self.device.wlan_policy.start_client_connections()
        self.device.wlan_policy.set_new_update_listener()
        asserts.assert_equal(
            self.device.wlan_policy.get_update(),
            ClientStateSummary(
                state=WlanClientState.CONNECTIONS_ENABLED,
                networks=[],
            ),
        )

        # Verify the access point came up
        asserts.assert_in(
            test_ssid,
            self.device.wlan_policy.scan_for_networks(),
            f'ssid "{test_ssid}" not found in scan results; check connection to the AP',
        )

        # Saving the network should initiate an auto-connection.
        self.device.wlan_policy.save_network(test_ssid, SecurityType.NONE)
        asserts.assert_equal(
            self.device.wlan_policy.get_saved_networks(),
            [NetworkConfig(test_ssid, SecurityType.NONE, "None", "")],
        )
        self.wait_for_network(test_ssid, ConnectionState.CONNECTING)
        self.wait_for_network(test_ssid, ConnectionState.CONNECTED)

        # Connecting explicitly again shouldn't do anything.
        self.device.wlan_policy.connect(test_ssid, SecurityType.NONE)
        for update in self.get_updates_until(timeout_sec=3):
            asserts.fail(f"Expected no updates, got {update}")

        # Stopping client connections should initiate a auto-disconnection.
        self.device.wlan_policy.stop_client_connections()
        asserts.assert_equal(
            self.device.wlan_policy.get_update(),
            ClientStateSummary(
                state=WlanClientState.CONNECTIONS_ENABLED,
                networks=[
                    NetworkState(
                        NetworkIdentifier(test_ssid, SecurityType.NONE),
                        ConnectionState.DISCONNECTED,
                        DisconnectStatus.CONNECTION_STOPPED,
                    )
                ],
            ),
        )
        asserts.assert_equal(
            self.device.wlan_policy.get_update(),
            ClientStateSummary(
                state=WlanClientState.CONNECTIONS_DISABLED,
                networks=[],
            ),
        )

        # Starting client connections again should initiate an auto-connection.
        self.device.wlan_policy.start_client_connections()
        self.wait_for_network(test_ssid, ConnectionState.CONNECTING)
        self.wait_for_network(test_ssid, ConnectionState.CONNECTED)

        # Removing the network should initiate a auto-disconnection.
        self.device.wlan_policy.remove_all_networks()
        asserts.assert_equal(self.device.wlan_policy.get_saved_networks(), [])
        self.wait_for_network(
            test_ssid,
            ConnectionState.DISCONNECTED,
            DisconnectStatus.CONNECTION_STOPPED,
        )

    def test_save_network_with_client_connections_disabled(self) -> None:
        """Verify save_network() works without enabling client connections."""
        self.device.wlan_policy.stop_client_connections()
        self.device.wlan_policy.set_new_update_listener()
        asserts.assert_equal(
            self.device.wlan_policy.get_update(),
            ClientStateSummary(
                state=WlanClientState.CONNECTIONS_DISABLED,
                networks=[],
            ),
        )

        test_ssid = random_str()
        self.device.wlan_policy.save_network(test_ssid, SecurityType.NONE)
        asserts.assert_equal(
            self.device.wlan_policy.get_saved_networks(),
            [NetworkConfig(test_ssid, SecurityType.NONE, "None", "")],
        )

        # Verify saving a network does not initiate an auto-connect.
        for update in self.get_updates_until(timeout_sec=3):
            asserts.fail(f"Expected no updates, got {update}")

    def test_connect_with_client_connections_disabled(self) -> None:
        """Verify connect() rejects without enabling client connections."""
        self.device.wlan_policy.stop_client_connections()
        self.device.wlan_policy.set_new_update_listener()
        asserts.assert_equal(
            self.device.wlan_policy.get_update(),
            ClientStateSummary(
                state=WlanClientState.CONNECTIONS_DISABLED,
                networks=[],
            ),
        )

        test_ssid = random_str()
        asserts.assert_equal(
            self.device.wlan_policy.connect(test_ssid, SecurityType.NONE),
            RequestStatus.REJECTED_NOT_SUPPORTED,
            "Connect requests should be rejected when client connections are "
            "disabled.",
        )

        # Verify connect doesn't change client state.
        for update in self.get_updates_until(timeout_sec=3):
            asserts.fail(f"Expected no updates, got {update}")

    def test_remove_all_networks_with_client_connections_disabled(self) -> None:
        """Verify remove_all_networks() works without enabling client
        connections."""
        self.device.wlan_policy.stop_client_connections()

        self.device.wlan_policy.remove_all_networks()
        asserts.assert_equal(
            self.device.wlan_policy.get_saved_networks(),
            [],
        )

        test_ssid = random_str()
        self.device.wlan_policy.save_network(test_ssid, SecurityType.NONE)
        asserts.assert_equal(
            self.device.wlan_policy.get_saved_networks(),
            [NetworkConfig(test_ssid, SecurityType.NONE, "None", "")],
        )

        self.device.wlan_policy.remove_all_networks()
        asserts.assert_equal(
            self.device.wlan_policy.get_saved_networks(),
            [],
        )

    def test_remove_network_with_client_connections_disabled(self) -> None:
        """Verify remove() works without enabling client connections."""
        test_ssid = random_str()

        # Removing a network that doesn't exist shouldn't error.
        self.device.wlan_policy.remove_network(test_ssid, SecurityType.NONE)
        asserts.assert_equal(
            self.device.wlan_policy.get_saved_networks(),
            [],
        )

        self.device.wlan_policy.save_network(test_ssid, SecurityType.NONE)
        asserts.assert_equal(
            self.device.wlan_policy.get_saved_networks(),
            [NetworkConfig(test_ssid, SecurityType.NONE, "None", "")],
        )

        self.device.wlan_policy.remove_network(test_ssid, SecurityType.NONE)
        asserts.assert_equal(
            self.device.wlan_policy.get_saved_networks(),
            [],
        )

    # TODO(http://b/339069764): Split WLAN utility functions out into a separate file
    def get_updates_until(
        self, timeout_sec: float = 5
    ) -> Iterator[ClientStateSummary]:
        """Iterate client state updates for a set duration."""
        end_time = time.time() + timeout_sec
        while time.time() < end_time:
            time_left = end_time - time.time()
            try:
                yield self.device.wlan_policy.get_update(timeout=time_left)
            except TimeoutError:
                return

    def wait_for_update(self, expected_update: ClientStateSummary) -> None:
        """Assert an update eventually matches the specified state."""
        last_updates: list[ClientStateSummary] = []

        for update in self.get_updates_until(DEFAULT_GET_UPDATE_TIMEOUT):
            if update == expected_update:
                return
            last_updates.append(update)

        asserts.fail(
            f"Timed out waiting {DEFAULT_GET_UPDATE_TIMEOUT}s for client "
            f"state: {expected_update}\n"
            f"Last updates: {last_updates}"
        )

    def wait_for_network(
        self,
        ssid: str,
        expected_state: ConnectionState,
        expected_status: DisconnectStatus | None = None,
        expected_client_state: WlanClientState = WlanClientState.CONNECTIONS_ENABLED,
    ) -> None:
        """Assert the next update matches the specified network state."""
        self.wait_for_update(
            ClientStateSummary(
                state=expected_client_state,
                networks=[
                    NetworkState(
                        NetworkIdentifier(ssid, SecurityType.NONE),
                        expected_state,
                        expected_status,
                    )
                ],
            )
        )


if __name__ == "__main__":
    test_runner.main()
