# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""WLAN policy access point affordance implementation using Fuchsia
Controller."""

import logging

from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod

from honeydew import errors
from honeydew.interfaces.affordances.wlan import wlan_policy_ap
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.wlan import (
    AccessPointState,
    ConnectivityMode,
    OperatingBand,
    SecurityType,
)

# List of required FIDLs for the affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.wlan.policy.AccessPointListener",
    "fuchsia.wlan.policy.AccessPointProvider",
    "fuchsia.wlan.phyimpl",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)


class WlanPolicyAp(AsyncAdapter, wlan_policy_ap.WlanPolicyAp):
    """WLAN affordance implemented with Fuchsia Controller."""

    def __init__(
        self,
        device_name: str,
        ffx: ffx_transport.FFX,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
        fuchsia_device_close: affordances_capable.FuchsiaDeviceClose,
    ) -> None:
        """Create a WLAN Policy Fuchsia Controller affordance.

        Args:
            device_name: Device name returned by `ffx target list`.
            ffx: FFX transport.
            fuchsia_controller: Fuchsia Controller transport.
            reboot_affordance: Object that implements RebootCapableDevice.
            fuchsia_device_close: Object that implements FuchsiaDeviceClose.
        """
        super().__init__()
        self._verify_supported(device_name, ffx)

        self._fc_transport = fuchsia_controller
        self._reboot_affordance = reboot_affordance
        self._fuchsia_device_close = fuchsia_device_close

        self._connect_proxy()
        self._reboot_affordance.register_for_on_device_boot(self._connect_proxy)

        self._fuchsia_device_close.register_for_on_device_close(self._close)

    def _close(self) -> None:
        """Release handle on ap controller.

        This needs to be called on test class teardown otherwise the device may
        be left in an inoperable state where no other components or tests can
        access state-changing WLAN Policy AP APIs.

        This is idempotent and irreversible. No other methods should be called
        after this one.
        """
        # TODO(http://b/324948461): Finish implementation

        if not self.loop().is_closed():
            self.loop().stop()
            # Allow the loop to finish processing pending tasks. This is
            # necessary to finish cancelling any tasks, which doesn't take long.
            self.loop().run_forever()
            self.loop().close()

    def _verify_supported(self, device: str, ffx: ffx_transport.FFX) -> None:
        """Check if WLAN Policy AP is supported on the DUT.

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
                    "All available WLAN component capabilities:\n%s",
                    ffx.run(["component", "capability", "fuchsia.wlan"]),
                )
                raise errors.NotSupportedError(
                    f'Component capability "{capability}" not exposed by device '
                    f"{device}; this build of Fuchsia does not support the "
                    "WLAN FC affordance."
                )

    def _connect_proxy(self) -> None:
        """Re-initializes connection to the WLAN stack."""
        # TODO(http://b/324948461): Finish implementation

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def start(
        self,
        ssid: str,
        security: SecurityType,
        password: str | None,
        mode: ConnectivityMode,
        band: OperatingBand,
    ) -> None:
        """Start an access point.

        Args:
            ssid: SSID of the network to start.
            security: The security protocol of the network.
            password: Credential used to connect to the network. None is
                equivalent to no password.
            mode: The connectivity mode to use
            band: The operating band to use

        Raises:
            HoneydewWlanError: Error from WLAN stack
            HoneydewWlanRequestRejectedError: WLAN rejected the request
        """
        # TODO(http://b/324948461): Finish implementation
        raise NotImplementedError()

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def stop(
        self,
        ssid: str,
        security: SecurityType,
        password: str | None,
    ) -> None:
        """Stop an active access point.

        Args:
            ssid: SSID of the network to stop.
            security: The security protocol of the network.
            password: Credential used to connect to the network. None is
                equivalent to no password.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            HoneydewWlanRequestRejectedError: WLAN rejected the request
        """
        # TODO(http://b/324948461): Finish implementation
        raise NotImplementedError()

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def stop_all(self) -> None:
        """Stop all active access points.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        # TODO(http://b/324948461): Finish implementation
        raise NotImplementedError()

    def set_new_update_listener(self) -> None:
        """Sets the update listener stream of the facade to a new stream.

        This causes updates to be reset. Intended to be used between tests so
        that the behavior of updates in a test is independent from previous
        tests.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        # TODO(http://b/324948461): Finish implementation
        raise NotImplementedError()

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def get_update(
        self,
        timeout: float | None = None,
    ) -> list[AccessPointState]:
        """Get a list of AP state listener updates.

        This call will return with an update immediately the
        first time the update listener is initialized by setting a new listener
        or by creating a client controller before setting a new listener.
        Subsequent calls will hang until there is a change since the last
        update call.

        Args:
            timeout: Timeout in seconds to wait for the get_update command to
                return. By default it is set to None (which means timeout is
                disabled)

        Returns:
            A list of AP state updates.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TimeoutError: Reached timeout without any updates.
        """
        # TODO(http://b/324948461): Finish implementation
        raise NotImplementedError()
