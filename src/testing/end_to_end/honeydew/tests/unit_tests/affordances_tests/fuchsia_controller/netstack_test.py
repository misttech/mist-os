# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.netstack."""

import unittest
from unittest import mock

from honeydew.affordances.fuchsia_controller import netstack
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport


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

    def test_init_connect_proxy(self) -> None:
        """Test if Netstack connects to fuchsia.net.interface proxies."""

    def test_list_interfaces(self) -> None:
        """Test if list_interfaces works."""
        with self.assertRaises(NotImplementedError):
            self.netstack_obj.list_interfaces()


if __name__ == "__main__":
    unittest.main()
