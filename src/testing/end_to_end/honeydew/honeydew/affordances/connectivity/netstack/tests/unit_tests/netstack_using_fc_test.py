# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.netstack."""

import asyncio
import unittest
from ipaddress import IPv4Address, IPv6Address
from typing import TypeVar
from unittest import mock

import fidl.fuchsia_net as f_net
import fidl.fuchsia_net_interfaces as f_net_interfaces
import fidl.fuchsia_net_root as f_net_root
from fuchsia_controller_py import Channel, ZxStatus

from honeydew.affordances.connectivity.netstack import netstack_using_fc
from honeydew.affordances.connectivity.netstack.types import (
    InterfaceProperties,
    PortClass,
)
from honeydew.affordances.connectivity.wlan.utils.types import MacAddress
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport

_TEST_MAC: MacAddress = MacAddress("12:34:56:78:90:ab")

_T = TypeVar("_T")


async def _async_response(response: _T) -> _T:
    return response


# pylint: disable=protected-access
class NetstackFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.netstack."""

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
            netstack_using_fc._REQUIRED_CAPABILITIES
        )

        self.netstack_obj = netstack_using_fc.NetstackUsingFc(
            device_name="fuchsia-emulator",
            ffx=self.ffx_transport_obj,
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )

        self.watcher: asyncio.Task[None] | None = None

    def tearDown(self) -> None:
        self.netstack_obj.loop().stop()
        self.netstack_obj.loop().run_forever()  # Handle pending tasks
        self.netstack_obj.loop().close()

    def test_verify_supported(self) -> None:
        """Test if _verify_supported works."""
        self.ffx_transport_obj.run.return_value = ""

        with self.assertRaises(NotSupportedError):
            self.netstack_obj = netstack_using_fc.NetstackUsingFc(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
            )

    def test_init_register_for_on_device_boot(self) -> None:
        """Test if Netstack registers on_device_boot."""
        self.reboot_affordance_obj.register_for_on_device_boot.assert_called_once_with(
            self.netstack_obj._connect_proxy
        )

    def test_init_connect_proxy(self) -> None:
        """Test if Netstack connects to fuchsia.net.interface/State."""
        self.assertIsNotNone(self.netstack_obj._state_proxy)

    def test_list_interfaces(self) -> None:
        """Test if list_interfaces works."""
        self.netstack_obj._state_proxy = mock.MagicMock(
            spec=f_net_interfaces.State.Client
        )

        def get_watcher(
            # pylint: disable-next=unused-argument
            options: f_net_interfaces.WatcherOptions,
            watcher: int,
        ) -> None:
            server = TestWatcherImpl(
                Channel(watcher),
                items=[
                    InterfaceProperties(
                        1,
                        "lo",
                        mac=_TEST_MAC,
                        ipv4_addresses=[IPv4Address("127.0.0.1")],
                        ipv6_addresses=[IPv6Address("fe80::1")],
                        port_class=PortClass.LOOPBACK,
                    ),
                    InterfaceProperties(
                        2,
                        "eth1",
                        mac=_TEST_MAC,
                        ipv4_addresses=[IPv4Address("192.168.42.1")],
                        ipv6_addresses=[],
                        port_class=PortClass.ETHERNET,
                    ),
                    InterfaceProperties(
                        3,
                        "wlan1",
                        mac=_TEST_MAC,
                        ipv4_addresses=[],
                        ipv6_addresses=[],
                        port_class=PortClass.WLAN_CLIENT,
                    ),
                ],
            )
            self.watcher = self.netstack_obj.loop().create_task(server.serve())

        self.netstack_obj._state_proxy.get_watcher = mock.Mock(
            wraps=get_watcher,
        )

        self.netstack_obj._interfaces_proxy = mock.MagicMock(
            spec=f_net_root.Interfaces.Client
        )

        mac_result = f_net_root.InterfacesGetMacResult()
        mac_result.response = f_net_root.InterfacesGetMacResponse(
            mac=f_net.MacAddress(
                octets=list(_TEST_MAC.bytes()),
            ),
        )

        self.netstack_obj._interfaces_proxy.get_mac.side_effect = [
            _async_response(mac_result),
            _async_response(mac_result),
            _async_response(mac_result),
        ]

        self.assertEqual(
            self.netstack_obj.list_interfaces(),
            [
                InterfaceProperties(
                    1,
                    "lo",
                    mac=_TEST_MAC,
                    ipv4_addresses=[IPv4Address("127.0.0.1")],
                    ipv6_addresses=[IPv6Address("fe80::1")],
                    port_class=PortClass.LOOPBACK,
                ),
                InterfaceProperties(
                    2,
                    "eth1",
                    mac=_TEST_MAC,
                    ipv4_addresses=[IPv4Address("192.168.42.1")],
                    ipv6_addresses=[],
                    port_class=PortClass.ETHERNET,
                ),
                InterfaceProperties(
                    3,
                    "wlan1",
                    mac=_TEST_MAC,
                    ipv4_addresses=[],
                    ipv6_addresses=[],
                    port_class=PortClass.WLAN_CLIENT,
                ),
            ],
        )


class TestWatcherImpl(f_net_interfaces.Watcher.Server):
    """Iterator for netstack events."""

    def __init__(
        self, server: Channel, items: list[InterfaceProperties]
    ) -> None:
        super().__init__(server)
        self._items = items
        self._done = False

    def watch(
        self,
    ) -> f_net_interfaces.WatcherWatchResponse:
        """Get next set of NetworkConfigs."""
        event = f_net_interfaces.Event()

        if len(self._items) == 0:
            if self._done:
                raise ZxStatus(ZxStatus.ZX_ERR_PEER_CLOSED)
            else:
                # Indicate no more existing events will be sent.
                event.idle = f_net_interfaces.Empty()
                self._done = True
        else:
            event.existing = self._items.pop(0).to_fidl()

        return f_net_interfaces.WatcherWatchResponse(event=event)


if __name__ == "__main__":
    unittest.main()
