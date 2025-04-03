# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Tests for connecting to an access point.
"""

import logging

logger = logging.getLogger(__name__)

import asyncio
import struct
import time
from dataclasses import dataclass
from typing import Any, List

import fidl_fuchsia_wlan_wlanix as fidl_wlanix
from antlion import utils
from antlion.controllers.access_point import setup_ap
from antlion.controllers.ap_lib.hostapd_constants import (
    AP_DEFAULT_CHANNEL_2G,
    AP_SSID_LENGTH_2G,
)
from antlion.controllers.ap_lib.hostapd_security import Security, SecurityMode
from antlion.controllers.ap_lib.hostapd_utils import generate_random_password
from fuchsia_controller_py import Channel
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod
from mobly import base_test, signals, test_runner
from mobly.asserts import assert_equal, assert_true, fail
from wlanix_testing import base_test


class ConnectToApTest(AsyncAdapter, base_test.ConnectionBaseTestClass):
    def pre_run(self) -> None:
        self.generate_tests(
            test_logic=self._test_logic,
            name_func=self.name_func,
            arg_sets=[
                (Security(security_mode=SecurityMode.OPEN),),
                (
                    Security(
                        security_mode=SecurityMode.WPA2,
                        password=generate_random_password(),
                    ),
                ),
            ],
        )

    def name_func(self, security: Security) -> str:
        return f"test_successfully_connect_to_ap_{security.security_mode}"

    @asyncmethod
    async def _test_logic(self, security: Security) -> None:
        ssid = utils.rand_ascii_str(AP_SSID_LENGTH_2G)
        password = getattr(security, "password", None)

        setup_ap(
            access_point=self.access_point(),
            profile_name="whirlwind",
            channel=AP_DEFAULT_CHANNEL_2G,
            ssid=ssid,
            security=security,
        )

        logger.info("Querying for IfaceIndex...")
        get_interface_message = fidl_wlanix.Nl80211Message(
            message_type=fidl_wlanix.Nl80211MessageType.MESSAGE,
            # fmt: off
            payload=[
                # Generic Netlink Header
                0x05,  # Command: GetInterface
                0x01,  # Version
                0x00, 0x00 # Reserved
            ],
            # fmt: on
        )
        response_list = (
            (await self.nl80211_proxy.message(message=get_interface_message))
            .unwrap()
            .responses
        )
        iface_index = await read_iface_index_or_fail(response_list)
        logger.info("Using IfaceIndex %d for connection test", iface_index)

        logger.info("Triggering a scan on IfaceIndex %d", iface_index)
        trigger_scan_message = fidl_wlanix.Nl80211Message(
            message_type=fidl_wlanix.Nl80211MessageType.MESSAGE,
            # fmt: off
            payload=[
                # Generic Netlink Header
                0x21,  # Command: TriggerScan
                0x01,  # Version
                0x00, 0x00,  # Reserved
                0x08, 0x00,  # Length
                0x03, 0x00,  # Type: IfaceIndex (little-endian)
                *list(struct.pack("<I", iface_index)),
            ],
            # fmt: on
        )
        response_list = (
            (await self.nl80211_proxy.message(message=trigger_scan_message))
            .unwrap()
            .responses
        )
        assert_equal(
            len(response_list),
            1,
            "Response from TriggerScan should contain a single ACK message.",
        )
        assert_equal(
            response_list[0].message_type,
            3,
            "Response should have been an ACK.",
        )

        # TODO(https://fxbug.dev/369151951): Implement a Nl80211Multicast server to
        # wait for the scan to complete.
        logger.info(
            "Waiting 5 seconds for scan to complete. See https://fxbug.dev/369151951"
        )
        time.sleep(5)

        with SupplicantStaIfaceCallbackServer() as ctx:
            state_change_queue = ctx.state_change_queue
            callback_proxy = ctx.callback_proxy

            self.supplicant_sta_iface_proxy.register_callback(
                callback=callback_proxy.channel.take()
            )

            proxy, server = Channel.create()
            self.supplicant_sta_iface_proxy.add_network(network=server.take())
            supplicant_sta_network_proxy = (
                fidl_wlanix.SupplicantStaNetworkClient(proxy)
            )

            supplicant_sta_network_proxy.set_ssid(
                ssid=list(ssid.encode("ascii"))
            )
            if password:
                supplicant_sta_network_proxy.set_psk_passphrase(
                    passphrase=list(password.encode("ascii"))
                )

            try:
                (await supplicant_sta_network_proxy.select()).unwrap()
            except RuntimeError as e:
                raise signals.TestFailure(
                    f'Failed to connect to "{ssid}" with {security}'
                ) from e
            logger.info(f'Successfully connected to "{ssid}"!')

            state_change = await state_change_queue.get()
            assert isinstance(
                state_change,
                fidl_wlanix.SupplicantStaIfaceCallbackOnStateChangedRequest,
            ), f"Expected OnStateChanged. Got {state_change!r}"
            assert_equal(
                state_change.new_state,
                fidl_wlanix.StaIfaceCallbackState.COMPLETED,
            )
            assert_true(
                state_change_queue.empty(),
                "Unexpectedly received additional callback messages.",
            )


@dataclass
class SupplicantStaIfaceCallbackContext:
    state_change_queue: asyncio.Queue[
        fidl_wlanix.SupplicantStaIfaceCallbackOnStateChangedRequest
        | fidl_wlanix.SupplicantStaIfaceCallbackOnDisconnectedRequest
        | fidl_wlanix.SupplicantStaIfaceCallbackOnAssociationRejectedRequest
    ]
    callback_proxy: fidl_wlanix.SupplicantStaIfaceCallbackClient


class SupplicantStaIfaceCallbackServer(
    fidl_wlanix.SupplicantStaIfaceCallbackServer
):
    def __init__(
        self,
        verbose: bool = True,
    ) -> None:
        self.verbose = verbose
        self.state_change_queue: asyncio.Queue[
            fidl_wlanix.SupplicantStaIfaceCallbackOnStateChangedRequest
            | fidl_wlanix.SupplicantStaIfaceCallbackOnDisconnectedRequest
            | fidl_wlanix.SupplicantStaIfaceCallbackOnAssociationRejectedRequest
        ] = asyncio.Queue()

    def on_state_changed(
        self,
        request: fidl_wlanix.SupplicantStaIfaceCallbackOnStateChangedRequest,
    ) -> None:
        if self.verbose:
            logger.info("State changed: %s", request)
        self.state_change_queue.put_nowait(request)

    def on_disconnected(
        self,
        request: fidl_wlanix.SupplicantStaIfaceCallbackOnDisconnectedRequest,
    ) -> None:
        if self.verbose:
            logger.info("Disconnected: %s", request)
        self.state_change_queue.put_nowait(request)

    def on_association_rejected(
        self,
        request: fidl_wlanix.SupplicantStaIfaceCallbackOnAssociationRejectedRequest,
    ) -> None:
        if self.verbose:
            logger.info("Association rejected: %s", request)
        self.state_change_queue.put_nowait(request)

    def __enter__(self) -> SupplicantStaIfaceCallbackContext:
        proxy, server = Channel.create()
        super().__init__(channel=server)
        self.server_task = asyncio.get_running_loop().create_task(self.serve())
        return SupplicantStaIfaceCallbackContext(
            state_change_queue=self.state_change_queue,
            callback_proxy=fidl_wlanix.SupplicantStaIfaceCallbackClient(proxy),
        )

    def __exit__(self, *args: Any, **kwargs: Any) -> None:
        if self.server_task:
            self.server_task.cancel()


async def read_iface_index_or_fail(
    response_list: List[fidl_wlanix.Nl80211Message],
) -> int:
    last_response_index = len(response_list) - 1
    for response_index, response in enumerate(response_list):
        if response.message_type == fidl_wlanix.Nl80211MessageType.DONE:
            assert_equal(
                response_index,
                last_response_index,
                "Nl80211 DONE message before end of response",
            )
            break

        if response.message_type in [
            fidl_wlanix.Nl80211MessageType.ERROR,
            fidl_wlanix.Nl80211MessageType.OVERRUN,
        ]:
            fail(
                "Received an error Nl80211 message type: %s",
                response.message_type,
            )

        if response.message_type in [
            fidl_wlanix.Nl80211MessageType.NO_OP,
            fidl_wlanix.Nl80211MessageType.ACK,
        ]:
            fail(
                "Received an unexpected Nl80211 message: %s",
                response.message_type,
            )

        assert_equal(
            response.message_type,
            fidl_wlanix.Nl80211MessageType.MESSAGE,
            "After filtering all other message types, a type other than MESSAGE was received.",
        )
        assert response.payload is not None, "MESSAGE must contain a payload"

        formatted_response_payload = [
            format(b, "#04x") for b in response.payload
        ]
        assert_equal(
            response.payload[0],
            7,
            f"Payload is not a NewInterface message: {formatted_response_payload}",
        )
        assert_equal(
            response.payload[6],
            3,
            f"First attribute is not an IfaceIndex: {formatted_response_payload}",
        )

        return struct.unpack("<I", bytes(response.payload[8:12]))[0]

    raise RuntimeError(
        f"Did not find an iface index in the response list: {response_list}"
    )


if __name__ == "__main__":
    test_runner.main()
