# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.netstack."""

import asyncio
import unittest
from ipaddress import IPv4Address
from unittest import mock

import fidl.fuchsia_net_interfaces as f_net_interfaces
from fuchsia_controller_py import Channel, ZxStatus

from honeydew.affordances.fuchsia_controller import netstack
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport
from honeydew.typing.netstack import InterfaceProperties


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
            netstack._REQUIRED_CAPABILITIES
        )

        self.netstack_obj = netstack.Netstack(
            device_name="fuchsia-emulator",
            ffx=self.ffx_transport_obj,
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )

        self.watcher: asyncio.Task[None] | None = None

    def test_verify_supported(self) -> None:
        """Test if _verify_supported works."""
        self.ffx_transport_obj.run.return_value = ""

        with self.assertRaises(NotSupportedError):
            self.netstack_obj = netstack.Netstack(
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
                        "wlan1",
                        ipv4_addresses=[IPv4Address("192.168.42.1")],
                        ipv6_addresses=[],
                    ),
                    InterfaceProperties(
                        2, "wlan2", ipv4_addresses=[], ipv6_addresses=[]
                    ),
                ],
            )
            self.watcher = self.netstack_obj.loop().create_task(server.serve())

        self.netstack_obj._state_proxy.get_watcher = mock.Mock(
            wraps=get_watcher,
        )

        self.assertEqual(
            self.netstack_obj.list_interfaces(),
            [
                InterfaceProperties(
                    1,
                    "wlan1",
                    ipv4_addresses=[IPv4Address("192.168.42.1")],
                    ipv6_addresses=[],
                ),
                InterfaceProperties(
                    2, "wlan2", ipv4_addresses=[], ipv6_addresses=[]
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
