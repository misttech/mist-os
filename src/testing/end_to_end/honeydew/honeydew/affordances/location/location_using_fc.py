# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Location affordance implementation using Fuchsia Controller."""

import logging

import fidl.fuchsia_location_namedplace as f_location_namedplace
from fuchsia_controller_py import ZxStatus

from honeydew import errors
from honeydew.affordances.location import location
from honeydew.affordances.location.errors import HoneydewLocationError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.custom_types import FidlEndpoint

# List of required FIDLs for this affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.location.namedplace",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)

# Fuchsia Controller proxies
_REGULATORY_REGION_CONFIGURATOR_PROXY = FidlEndpoint(
    "core/regulatory_region",
    "fuchsia.location.namedplace.RegulatoryRegionConfigurator",
)


class LocationUsingFc(location.Location):
    """Location affordance implemented with Fuchsia Controller."""

    def __init__(
        self,
        device_name: str,
        ffx: ffx_transport.FFX,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
    ) -> None:
        """Create a location Fuchsia Controller affordance.

        Args:
            device_name: Device name returned by `ffx target list`.
            ffx: FFX transport.
            fuchsia_controller: Fuchsia Controller transport.
            reboot_affordance: Object that implements RebootCapableDevice.
        """
        super().__init__()
        self._verify_supported(device_name, ffx)

        self._fc_transport = fuchsia_controller
        self._reboot_affordance = reboot_affordance

        self._connect_proxy()
        self._reboot_affordance.register_for_on_device_boot(self._connect_proxy)

    def _verify_supported(self, device: str, ffx: ffx_transport.FFX) -> None:
        """Check if location is supported on the DUT.

        Args:
            device: Device name returned by `ffx target list`.
            ffx: FFX transport

        Raises:
            NotSupportedError: A required component capability is not available.
        """
        for capability in _REQUIRED_CAPABILITIES:
            # TODO(http://b/359342196): This is a maintenance burden; find a
            # better way to detect FIDL component capabilities.
            if capability not in ffx.run(
                ["component", "capability", capability]
            ):
                _LOGGER.warning(
                    "All available location component capabilities:\n%s",
                    ffx.run(["component", "capability", "fuchsia.location"]),
                )
                raise errors.NotSupportedError(
                    f'Component capability "{capability}" not exposed by device '
                    f"{device}; this build of Fuchsia does not support the "
                    "location affordance."
                )

    def _connect_proxy(self) -> None:
        """Re-initializes connection to the location stack."""
        self._regulatory_region_configurator = (
            f_location_namedplace.RegulatoryRegionConfigurator.Client(
                self._fc_transport.connect_device_proxy(
                    _REGULATORY_REGION_CONFIGURATOR_PROXY
                )
            )
        )

    def set_region(self, region_code: str) -> None:
        """Set regulatory region.

        Args:
            region_code: 2-byte ASCII string.

        Raises:
            HoneydewLocationError: Error from location stack
            TypeError: Invalid region_code format
        """
        if len(region_code) != 2:
            raise TypeError(
                f'Expected region_code to be length 2, got "{region_code}"'
            )

        try:
            self._regulatory_region_configurator.set_region(region=region_code)
        except ZxStatus as status:
            _LOGGER.error("set_region zxstatus error = %s", status)
            raise HoneydewLocationError(
                f"RegulatoryRegionConfigurator.SetRegion() error {status}"
            ) from status

        # TODO(http://b/370600007): Validate region was set using
        # RegulatoryRegionWatcher.GetRegionUpdate().
