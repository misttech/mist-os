#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for wlan policy affordance."""

import logging
import time

from antlion.controllers import access_point
from antlion.controllers.ap_lib import hostapd_constants
from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, signals, test_runner

from honeydew import errors
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing.wlan import (
    ConnectionState,
    DisconnectStatus,
    NetworkState,
    RequestStatus,
    SecurityType,
    WlanClientState,
)

_LOGGER: logging.Logger = logging.getLogger(__name__)
DEFAULT_GET_UPDATE_TIMEOUT = 60


# TODO(http://b/339069764): Split these WLAN utility functions out into a separate file
def _find_network(
    ssid: str, networks: list[NetworkState]
) -> NetworkState | None:
    """Helper method to find network in list of network states.

    Args:
        ssid: The network name to look for.
        networks: The list of network states to look in.

    Returns:
        Network state of target ssid or None if not found in networks.
    """
    for network in networks:
        if network.network_identifier.ssid == ssid:
            return network
    return None


def _wait_for_network_state(
    device: fuchsia_device.FuchsiaDevice,
    ssid: str,
    expected_state: ConnectionState,
    expected_status: DisconnectStatus | None = None,
    timeout_sec: int = DEFAULT_GET_UPDATE_TIMEOUT,
) -> bool:
    """Waits until the device returns with expected network state.

    Args:
        device: Fuchsia wlan device under test.
        ssid: The network name to check the state of.
        expected_state: The network state we are waiting to see.
        expected_status: The disconnect status of the network.
        timeout_sec: The number of seconds to wait for a update showing connection.

    Returns:
        True if network converges on expected_state and expected_status if given False
            otherwise.
    """
    device.wlan_policy.set_new_update_listener()

    end_time = time.time() + timeout_sec
    while time.time() < end_time:
        time_left = max(1, int(end_time - time.time()))
        try:
            client_state = device.wlan_policy.get_update(timeout=time_left)
        except errors.Sl4fError:
            # WlanPolicyError can be thrown if the SL4F command was not successfully
            # sent, if the command timed out, or if the command returned with an
            # error code in the 'error' field. We retry here to handle the cases
            # in negative testing where we expect to receive an 'error'.
            time.sleep(1)
            continue

        network = _find_network(ssid, client_state.networks)
        if network is None:
            _LOGGER.info(f"{ssid} not found in client networks")
            time.sleep(1)
            continue

        if network.connection_state is not expected_state:
            _LOGGER.info(
                f'Expected connection state "{expected_state}", '
                f'got "{network.connection_state}"'
            )
            time.sleep(1)
            continue

        # If an expected status is given we check after reaching expected state.
        match network.connection_state:
            case ConnectionState.FAILED | ConnectionState.DISCONNECTED:
                if (
                    expected_status
                    and network.disconnect_status is not expected_status
                ):
                    _LOGGER.info(
                        f'Expected status "{expected_status}", '
                        f'got "{network.disconnect_status}"'
                    )
                    return False
            case ConnectionState.CONNECTED | ConnectionState.CONNECTING:
                # Normally these network states do not have disconnect status, but
                # we are setting a default value to CONNECTION_STOPPED
                if (
                    network.disconnect_status
                    is not DisconnectStatus.CONNECTION_STOPPED
                ):
                    _LOGGER.info(
                        f'Expected status "{expected_status}", '
                        f'got "{network.disconnect_status}"'
                    )
                    return False

        # Successfully converged on expected state/status
        return True
    else:
        _LOGGER.info(
            f'Timed out waiting for "{ssid}" to reach state {expected_state} and '
            f"status {expected_status}"
        )
        return False


def _wait_for_client_state(
    device: fuchsia_device.FuchsiaDevice,
    expected_state: WlanClientState,
    timeout_sec: int = DEFAULT_GET_UPDATE_TIMEOUT,
) -> bool:
    """Waits until the client converges to expected state.

    Args:
        device: Fuchsia wlan device under test.
        expected_state: The client state we are waiting to see.
        timeout_sec: Duration to wait for the desired_state.

    Returns:
        True if client converges on expected_state False otherwise.
    """
    device.wlan_policy.set_new_update_listener()

    end_time = time.time() + timeout_sec
    while time.time() < end_time:
        time_left = max(1, int(end_time - time.time()))
        try:
            client_state = device.wlan_policy.get_update(timeout=time_left)
        except errors.Sl4fError:
            # WlanPolicyError can be thrown if the SL4F command was not successfully
            # sent, if the command timed out, or if the command returned with an
            # error code in the 'error' field. We retry here to handle the cases
            # in negative testing where we expect to receive an 'error'.
            time.sleep(1)
            continue

        if client_state.state is not expected_state:
            # Continue getting updates.
            time.sleep(1)
            continue
        else:
            return True
    else:
        _LOGGER.info(
            f"Client state did not converge to the expected state: {expected_state}"
            f" Waited:{timeout_sec}s"
        )
        return False


class WlanPolicyTests(fuchsia_base_test.FuchsiaBaseTest):
    """WlanPolicy affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
            * Assigns `access_point` variable with AccessPoint object
            * Creates the client controller needed for wlan operations
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self.device.wlan_policy.create_client_controller()

        self.access_points: list[
            access_point.AccessPoint
        ] = self.register_controller(access_point)

        if len(self.access_points) < 1:
            raise signals.TestAbortClass(
                "At least one access point is required"
            )

        self.access_point: access_point.AccessPoint = self.access_points[0]

    def test_client_methods(self) -> None:
        """Test case for wlan_policy client methods.

        This test starts and stops client connections and checks that they are in the
        expected states.

        This test case calls the following wlan_policy methods:
            * wlan_policy.create_client_controller()
            * wlan_policy.set_new_update_listener() - called in wlan_utils
            * wlan_policy.get_update() - called in wlan_utils
            * wlan_policy.start_client_connections()
            * wlan_policy.stop_client_connections()
        """
        self.device.wlan_policy.start_client_connections()
        _wait_for_client_state(self.device, WlanClientState.CONNECTIONS_ENABLED)
        self.device.wlan_policy.stop_client_connections()
        _wait_for_client_state(
            self.device, WlanClientState.CONNECTIONS_DISABLED
        )

    def test_network_methods(self) -> None:
        """Test case for wlan_policy network methods.

        This test sets up a test network with the access point. It then scans for, saves
        and connects to the that network. We don't need to verify save_network
        explicitly because connect would fail if save_network failed. The test then
        removes that network using both single network removal and all network removal
        and checks that the network is not present in the saved networks after each
        time.

        This test case calls the following wlan_policy methods:
            * wlan_policy.scan_for_networks()
            * wlan_policy.save_network()
            * wlan_policy.connect()
            * wlan_policy.set_new_update_listener() - called in wlan_utils
            * wlan_policy.get_update() - called in wlan_utils
            * wlan_policy.remove_network()
            * wlan_policy.remove_all_networks()
            * wlan_policy.get_saved_networks()
        """
        test_ssid = "test"
        access_point.setup_ap(
            access_point=self.access_point,
            profile_name="whirlwind",
            channel=hostapd_constants.AP_DEFAULT_CHANNEL_2G,
            ssid=test_ssid,
        )
        expected_networks = [test_ssid]

        self.device.wlan_policy.start_client_connections()
        _wait_for_client_state(self.device, WlanClientState.CONNECTIONS_ENABLED)
        self.device.wlan_policy.save_network(test_ssid, SecurityType.NONE)

        scan_results = self.device.wlan_policy.scan_for_networks()
        asserts.assert_equal(sorted(scan_results), sorted(expected_networks))

        connect_resp = self.device.wlan_policy.connect(
            test_ssid, SecurityType.NONE
        )
        asserts.assert_equal(connect_resp, RequestStatus.ACKNOWLEDGED)
        _wait_for_network_state(
            self.device, test_ssid, ConnectionState.CONNECTED
        )

        self.device.wlan_policy.remove_network(test_ssid, SecurityType.NONE)
        networks = self.device.wlan_policy.get_saved_networks()
        asserts.assert_equal(networks, [])

        self.device.wlan_policy.save_network(test_ssid, SecurityType.NONE)
        self.device.wlan_policy.remove_all_networks()
        networks = self.device.wlan_policy.get_saved_networks()
        asserts.assert_equal(networks, [])


if __name__ == "__main__":
    test_runner.main()
