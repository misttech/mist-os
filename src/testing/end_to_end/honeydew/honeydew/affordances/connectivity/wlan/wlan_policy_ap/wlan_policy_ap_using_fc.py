# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""WLAN policy access point affordance implementation using Fuchsia
Controller."""

import asyncio
import logging
from dataclasses import dataclass

import fidl.fuchsia_wlan_policy as f_wlan_policy
from fuchsia_controller_py import Channel, ZxStatus
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod

from honeydew import errors
from honeydew.affordances.connectivity.wlan.utils import errors as wlan_errors
from honeydew.affordances.connectivity.wlan.utils.types import (
    AccessPointState,
    ConnectivityMode,
    Credential,
    NetworkConfig,
    OperatingBand,
    RequestStatus,
    SecurityType,
)
from honeydew.affordances.connectivity.wlan.wlan_policy_ap import wlan_policy_ap
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.custom_types import FidlEndpoint

# List of required FIDLs for the affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.wlan.policy.AccessPointListener",
    "fuchsia.wlan.policy.AccessPointProvider",
    "fuchsia.wlan.phyimpl",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)

# Fuchsia Controller proxies
_ACCESS_POINT_PROVIDER_PROXY = FidlEndpoint(
    "core/wlancfg", "fuchsia.wlan.policy.AccessPointProvider"
)
_ACCESS_POINT_LISTENER_PROXY = FidlEndpoint(
    "core/wlancfg", "fuchsia.wlan.policy.AccessPointListener"
)


@dataclass
class _AccessPointControllerState:
    proxy: f_wlan_policy.AccessPointController.Client
    updates: asyncio.Queue[list[AccessPointState]]
    # Keep the async task for fuchsia.wlan.policy/AccessPointStateUpdates so it
    # doesn't get garbage collected when cancelled.
    access_point_state_updates_server_task: asyncio.Task[None]


class WlanPolicyAp(AsyncAdapter, wlan_policy_ap.WlanPolicyAp):
    """WlanPolicyAp affordance implemented with Fuchsia Controller."""

    def __init__(
        self,
        device_name: str,
        ffx: ffx_transport.FFX,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
        fuchsia_device_close: affordances_capable.FuchsiaDeviceClose,
    ) -> None:
        """Create a WlanPolicyAp Fuchsia Controller affordance.

        Args:
            device_name: Device name returned by `ffx target list`.
            ffx: FFX transport.
            fuchsia_controller: Fuchsia Controller transport.
            reboot_affordance: Object that implements RebootCapableDevice.
            fuchsia_device_close: Object that implements FuchsiaDeviceClose.
        """
        super().__init__()

        self._device_name: str = device_name
        self._ffx: ffx_transport.FFX = ffx
        self._fc_transport = fuchsia_controller
        self._reboot_affordance = reboot_affordance
        self._fuchsia_device_close = fuchsia_device_close

        self.verify_supported()

        self._connect_proxy()
        self._reboot_affordance.register_for_on_device_boot(self._connect_proxy)
        self._fuchsia_device_close.register_for_on_device_close(self._close)

        self._access_point_controller = self._create_access_point_controller()

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

    def verify_supported(self) -> None:
        """Verifies that the WlanPolicyAp affordance using FuchsiaController is supported by the
        Fuchsia device.

        This method should be called in `__init__()` so that if this affordance was called on a
        Fuchsia device that does not support it, it will raise NotSupportedError.

        Raises:
            NotSupportedError: If affordance is not supported.
        """
        for capability in _REQUIRED_CAPABILITIES:
            # TODO(http://b/359342196): This is a maintenance burden; find a
            # better way to detect FIDL component capabilities.
            if capability not in self._ffx.run(
                ["component", "capability", capability]
            ):
                _LOGGER.warning(
                    "All available WLAN component capabilities:\n%s",
                    self._ffx.run(["component", "capability", "fuchsia.wlan"]),
                )
                raise errors.NotSupportedError(
                    f'Component capability "{capability}" not exposed by device '
                    f"{self._device_name}; this build of Fuchsia does not support the "
                    "WLAN FC affordance."
                )

    def _connect_proxy(self) -> None:
        """Re-initializes connection to the WLAN stack."""
        self._access_point_provider_proxy = (
            f_wlan_policy.AccessPointProvider.Client(
                self._fc_transport.connect_device_proxy(
                    _ACCESS_POINT_PROVIDER_PROXY
                )
            )
        )
        self._access_point_listener_proxy = (
            f_wlan_policy.AccessPointListener.Client(
                self._fc_transport.connect_device_proxy(
                    _ACCESS_POINT_LISTENER_PROXY
                )
            )
        )

    def _create_access_point_controller(self) -> _AccessPointControllerState:
        """Initialize the access point controller.

        See fuchsia.wlan.policy/AccessPointProvider.GetController().

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        controller_client, controller_server = Channel.create()
        access_point_controller_proxy = (
            f_wlan_policy.AccessPointController.Client(controller_client.take())
        )

        updates: asyncio.Queue[list[AccessPointState]] = asyncio.Queue()

        updates_client, updates_server = Channel.create()
        access_point_state_updates_server = AccessPointStateUpdatesImpl(
            updates_server, updates
        )
        task = self.loop().create_task(
            access_point_state_updates_server.serve()
        )

        try:
            self._access_point_provider_proxy.get_controller(
                requests=controller_server.take(),
                updates=updates_client.take(),
            )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"AccessPointProvider.GetController() error {status}"
            ) from status

        return _AccessPointControllerState(
            proxy=access_point_controller_proxy,
            updates=updates,
            access_point_state_updates_server_task=task,
        )

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
        cred = Credential.from_password(password)

        try:
            resp = await self._access_point_controller.proxy.start_access_point(
                config=NetworkConfig(
                    ssid, security, cred.type(), cred.value()
                ).to_fidl(),
                mode=mode.to_fidl(),
                band=band.to_fidl(),
            )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"AccessPointController.StartAccessPoint() error {status}"
            ) from status

        request_status = RequestStatus.from_fidl(resp.status)
        if request_status is not RequestStatus.ACKNOWLEDGED:
            raise wlan_errors.HoneydewWlanRequestRejectedError(
                "AccessPointController.StartAccessPoint()",
                request_status,
            )

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
        cred = Credential.from_password(password)

        try:
            resp = await self._access_point_controller.proxy.stop_access_point(
                config=NetworkConfig(
                    ssid, security, cred.type(), cred.value()
                ).to_fidl(),
            )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"AccessPointController.StopAccessPoint() error {status}"
            ) from status

        request_status = RequestStatus.from_fidl(resp.status)
        if request_status is not RequestStatus.ACKNOWLEDGED:
            raise wlan_errors.HoneydewWlanRequestRejectedError(
                "AccessPointController.StopAccessPoint()",
                request_status,
            )

    def stop_all(self) -> None:
        """Stop all active access points.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        self._access_point_controller.proxy.stop_all_access_points()

    def set_new_update_listener(self) -> None:
        """Sets the update listener stream of the facade to a new stream.

        This causes updates to be reset. Intended to be used between tests so
        that the behavior of updates in a test is independent from previous
        tests.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        # Replace the existing AccessPointStateUpdates server without giving up our
        # handle to AccessPointController. This is necessary since the AccessPointProvider
        # API is designed to only allow a single caller to make AccessPointController
        # calls which would impact WLAN state. If we lose our handle to the
        # AccessPointController, some other component on the system could take it.
        self.cancel_task(
            self._access_point_controller.access_point_state_updates_server_task
        )

        access_point_listener_proxy = f_wlan_policy.AccessPointListener.Client(
            self._fc_transport.connect_device_proxy(
                _ACCESS_POINT_LISTENER_PROXY
            )
        )

        updates: asyncio.Queue[list[AccessPointState]] = asyncio.Queue()
        updates_client, updates_server = Channel.create()
        access_point_state_updates_server = AccessPointStateUpdatesImpl(
            updates_server, updates
        )
        task = self._async_adapter_loop.create_task(
            access_point_state_updates_server.serve()
        )

        try:
            access_point_listener_proxy.get_listener(
                updates=updates_client.take(),
            )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"AccessPointListener.GetListener() error {status}"
            ) from status

        self._access_point_controller.updates = updates
        self._access_point_controller.access_point_state_updates_server_task = (
            task
        )

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
        return await asyncio.wait_for(
            self._access_point_controller.updates.get(), timeout
        )


class AccessPointStateUpdatesImpl(f_wlan_policy.AccessPointStateUpdates.Server):
    """Server to receive WLAN access point state changes.

    Receive updates on the current summary of wlan access point operating
    states. This will be called when there are changes with active access point
    networks - both the number of access points and their individual activity.
    """

    def __init__(
        self, server: Channel, updates: asyncio.Queue[list[AccessPointState]]
    ) -> None:
        super().__init__(server)
        self._updates = updates
        _LOGGER.debug("Started AccessPointStateUpdates server")

    async def on_access_point_state_update(
        self,
        request: f_wlan_policy.AccessPointStateUpdatesOnAccessPointStateUpdateRequest,
    ) -> None:
        """Detected a change to the state or registered listeners.

        Args:
            request: Current summary of WLAN access point operating states.
        """
        access_points = [
            AccessPointState.from_fidl(ap) for ap in request.access_points
        ]
        _LOGGER.debug(
            "OnAccessPointStateUpdates called with %s", repr(access_points)
        )
        await self._updates.put(access_points)
