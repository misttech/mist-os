# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.location."""

import unittest
from typing import TypeVar
from unittest import mock

import fidl.fuchsia_location_namedplace as f_location_namedplace
from fuchsia_controller_py import ZxStatus

from honeydew.affordances.location import location_using_fc
from honeydew.affordances.location.errors import HoneydewLocationError
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport

_T = TypeVar("_T")


async def _async_response(response: _T) -> _T:
    return response


# pylint: disable=protected-access
class LocationFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.location."""

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
            location_using_fc._REQUIRED_CAPABILITIES
        )

        self.location_obj = location_using_fc.LocationUsingFc(
            device_name="fuchsia-emulator",
            ffx=self.ffx_transport_obj,
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )

    def test_verify_supported(self) -> None:
        """Test if _verify_supported works."""
        self.ffx_transport_obj.run.return_value = ""

        with self.assertRaises(NotSupportedError):
            self.location_obj = location_using_fc.LocationUsingFc(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
            )

    def test_init_register_for_on_device_boot(self) -> None:
        """Test if Location registers on_device_boot."""
        self.reboot_affordance_obj.register_for_on_device_boot.assert_called_once_with(
            self.location_obj._connect_proxy
        )

    def test_init_connect_proxy(self) -> None:
        """Test if Location connects to
        fuchsia.location.namedplace/RegulatoryRegionConfigurator."""
        self.assertIsNotNone(self.location_obj._regulatory_region_configurator)

    def test_set_region_works(self) -> None:
        """Test if set_region works with valid input."""
        self.location_obj._regulatory_region_configurator = mock.MagicMock(
            spec=f_location_namedplace.RegulatoryRegionConfigurator.Client
        )
        self.location_obj._regulatory_region_configurator.set_region.return_value = (
            None
        )
        self.location_obj.set_region("AT")

    def test_set_region_fails_internal_error(self) -> None:
        """Verify set_region fails when the location stack errors."""
        self.location_obj._regulatory_region_configurator = mock.MagicMock(
            spec=f_location_namedplace.RegulatoryRegionConfigurator.Client
        )
        self.location_obj._regulatory_region_configurator.set_region.side_effect = ZxStatus(
            ZxStatus.ZX_ERR_INTERNAL
        )
        with self.assertRaises(HoneydewLocationError):
            self.location_obj.set_region("AT")

    def test_set_region_fails_invalid_region_code(self) -> None:
        """Verify set_region fails immediately with invalid input."""
        with self.assertRaises(TypeError):
            self.location_obj.set_region("?")


if __name__ == "__main__":
    unittest.main()
