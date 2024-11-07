# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""WLAN policy affordance implementation using Fuchsia Controller."""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass
from typing import Protocol, TypeVar

import fidl.fuchsia_wlan_policy as f_wlan_policy
from fuchsia_controller_py import Channel, ZxStatus
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod

from honeydew import errors
from honeydew.affordances.connectivity.wlan.utils import errors as wlan_errors
from honeydew.affordances.connectivity.wlan.utils.types import (
    ClientStateSummary,
    Credential,
    NetworkConfig,
    NetworkIdentifier,
    RequestStatus,
    SecurityType,
)
from honeydew.affordances.connectivity.wlan.wlan_policy import wlan_policy
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.custom_types import FidlEndpoint

# List of required FIDLs for the WLAN Fuchsia Controller affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.wlan.policy.ClientListener",
    "fuchsia.wlan.policy.ClientProvider",
    "fuchsia.wlan.phyimpl",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)

# Fuchsia Controller proxies
_CLIENT_PROVIDER_PROXY = FidlEndpoint(
    "core/wlancfg", "fuchsia.wlan.policy.ClientProvider"
)
_CLIENT_LISTENER_PROXY = FidlEndpoint(
    "core/wlancfg", "fuchsia.wlan.policy.ClientListener"
)


_T_co = TypeVar("_T_co", covariant=True)


class AsyncIterator(Protocol[_T_co]):
    """A FIDL iterator."""

    async def get_next(self) -> _T_co:
        """Get the next element."""


async def collect_iterator(
    iterator: AsyncIterator[_T_co], err_type: type | None = None
) -> list[_T_co]:
    """Collect all elements from a FIDL iterator.

    Will check for errors during collection.

    Args:
        iterator: Iterator to collect elements from.
        err_type: Error class for the result error. Defaults to None for no
            error checking.

    Returns:
        All elements collected from iterator.

    Raises:
        HoneydewWlanError: Error from WLAN stack.
    """
    elements: list[_T_co] = []
    while True:
        try:
            res = await iterator.get_next()
        except ZxStatus as status:
            if status.raw() == ZxStatus.ZX_ERR_PEER_CLOSED:
                # The server closed the channel, signifying the end of elements.
                break
            else:
                raise wlan_errors.HoneydewWlanError(
                    f"{type(iterator).__name__}.GetNext() error {status}"
                ) from status

        # Check for error
        err = getattr(res, "err", None)
        if err and err_type:
            raise wlan_errors.HoneydewWlanError(
                f"{type(iterator).__name__}.GetNext() {err_type.__name__} {err_type(err).name}"
            )

        elements.append(res)

    return elements


@dataclass
class ClientControllerState:
    proxy: f_wlan_policy.ClientController.Client
    updates: asyncio.Queue[ClientStateSummary]
    # Keep the async task for fuchsia.wlan.policy/ClientStateUpdates so it
    # doesn't get garbage collected then cancelled.
    client_state_updates_server_task: asyncio.Task[None]


class WlanPolicy(AsyncAdapter, wlan_policy.WlanPolicy):
    """WlanPolicy affordance implemented with Fuchsia Controller."""

    def __init__(
        self,
        device_name: str,
        ffx: ffx_transport.FFX,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
        fuchsia_device_close: affordances_capable.FuchsiaDeviceClose,
    ) -> None:
        """Create a WlanPolicy Fuchsia Controller affordance.

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
        self._client_controller: ClientControllerState | None = None

        self.verify_supported()

        self._connect_proxy()
        self._reboot_affordance.register_for_on_device_boot(self._connect_proxy)

        self._fuchsia_device_close.register_for_on_device_close(self._close)

    def _close(self) -> None:
        """Release handle on client controller.

        This needs to be called on test class teardown otherwise the device may
        be left in an inoperable state where no other components or tests can
        access state-changing WLAN Policy APIs.

        This is idempotent and irreversible. No other methods should be called
        after this one.
        """
        if self._client_controller:
            self.cancel_task(
                self._client_controller.client_state_updates_server_task
            )
            self._client_controller = None

        if not self.loop().is_closed():
            self.loop().stop()
            self.loop().run_forever()  # Handle pending tasks
            self.loop().close()

    def verify_supported(self) -> None:
        """Verifies that the WlanPolicy affordance using FuchsiaController is supported by the
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
        self._client_provider_proxy = f_wlan_policy.ClientProvider.Client(
            self._fc_transport.connect_device_proxy(_CLIENT_PROVIDER_PROXY)
        )

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def connect(
        self, target_ssid: str, security_type: SecurityType
    ) -> RequestStatus:
        """Triggers connection to a network.

        Args:
            target_ssid: The network to connect to. Must have been previously
                saved in order for a successful connection to happen.
            security_type: The security protocol of the network.

        Returns:
            A RequestStatus response to the connect request

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TypeError: Return value not a string.
            RuntimeError: Client controller has not been initialized
        """
        if self._client_controller is None:
            raise RuntimeError(
                "Client controller has not been initialized; call "
                "create_client_controller() before connect()"
            )

        try:
            resp = await self._client_controller.proxy.connect(
                id=NetworkIdentifier(target_ssid, security_type).to_fidl(),
            )
            return RequestStatus.from_fidl(resp.status)
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientController.Connect() error {status}"
            ) from status

    def create_client_controller(self) -> None:
        """Initializes the client controller.

        See fuchsia.wlan.policy/ClientProvider.GetController().

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        if self._client_controller:
            self.cancel_task(
                self._client_controller.client_state_updates_server_task
            )
            self._client_controller = None

        controller_client, controller_server = Channel.create()
        client_controller_proxy = f_wlan_policy.ClientController.Client(
            controller_client.take()
        )

        updates: asyncio.Queue[ClientStateSummary] = asyncio.Queue()

        updates_client, updates_server = Channel.create()
        client_state_updates_server = ClientStateUpdatesImpl(
            updates_server, updates
        )
        task = self.loop().create_task(client_state_updates_server.serve())

        try:
            self._client_provider_proxy.get_controller(
                requests=controller_server.take(),
                updates=updates_client.take(),
            )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientProvider.GetController() error {status}"
            ) from status

        self._client_controller = ClientControllerState(
            proxy=client_controller_proxy,
            updates=updates,
            client_state_updates_server_task=task,
        )

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def get_saved_networks(self) -> list[NetworkConfig]:
        """Gets networks saved on device.

        Returns:
            A list of NetworkConfigs.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TypeError: Return values not correct types.
            RuntimeError: Client controller has not been initialized
        """
        if self._client_controller is None:
            raise RuntimeError(
                "Client controller has not been initialized; call "
                "create_client_controller() before get_saved_networks()"
            )

        client, server = Channel.create()
        iterator = f_wlan_policy.NetworkConfigIterator.Client(client.take())

        try:
            self._client_controller.proxy.get_saved_networks(
                iterator=server.take(),
            )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientController.GetSavedNetworks() error {status}"
            ) from status

        return [
            NetworkConfig.from_fidl(config)
            for resp in await collect_iterator(
                iterator, f_wlan_policy.ScanErrorCode
            )
            for config in resp.configs
        ]

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def get_update(
        self,
        timeout: float | None = None,
    ) -> ClientStateSummary:
        """Gets one client listener update.

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
            An update of connection status. If there is no error, the result is
            a WlanPolicyUpdate with a structure that matches the FIDL
            ClientStateSummary struct given for updates.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TimeoutError: Reached timeout without any updates.
        """
        if self._client_controller is None:
            self.create_client_controller()
            assert self._client_controller is not None

        return await asyncio.wait_for(
            self._client_controller.updates.get(), timeout
        )

    def remove_all_networks(self) -> None:
        """Deletes all saved networks on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            RuntimeError: A client controller has not been created yet
        """
        if self._client_controller is None:
            raise RuntimeError(
                "Client controller has not been initialized; call "
                "create_client_controller() before remove_all_networks()"
            )

        for network in self.get_saved_networks():
            self.remove_network(
                target_ssid=network.ssid,
                security_type=network.security_type,
                target_pwd=network.credential_value,
            )

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def remove_network(
        self,
        target_ssid: str,
        security_type: SecurityType,
        target_pwd: str | None = None,
    ) -> None:
        """Removes or "forgets" a network from saved networks.

        Args:
            target_ssid: The network to remove.
            security_type: The security protocol of the network.
            target_pwd: The credential being saved with the network. No password
                is equivalent to an empty string.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            RuntimeError: A client controller has not been created yet
        """
        if self._client_controller is None:
            raise RuntimeError(
                "Client controller has not been initialized; call "
                "create_client_controller() before remove_network()"
            )

        try:
            res = await self._client_controller.proxy.remove_network(
                config=f_wlan_policy.NetworkConfig(
                    id=f_wlan_policy.NetworkIdentifier(
                        ssid=list(target_ssid.encode("utf-8")),
                        type=security_type.to_fidl(),
                    ),
                    credential=Credential.from_password(target_pwd).to_fidl(),
                ),
            )
            if res.err:
                raise wlan_errors.HoneydewWlanError(
                    "ClientController.SaveNetworks() NetworkConfigChangeError "
                    f"{res.err.name}"
                )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientController.SaveNetwork() error {status}"
            ) from status

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def save_network(
        self,
        target_ssid: str,
        security_type: SecurityType,
        target_pwd: str | None = None,
    ) -> None:
        """Saves a network to the device.

        Args:
            target_ssid: The network to save.
            security_type: The security protocol of the network.
            target_pwd: The credential being saved with the network. No password
                is equivalent to an empty string.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            RuntimeError: A client controller has not been created yet
        """
        if self._client_controller is None:
            raise RuntimeError(
                "Client controller has not been initialized; call "
                "create_client_controller() before save_network()"
            )

        try:
            res = await self._client_controller.proxy.save_network(
                config=f_wlan_policy.NetworkConfig(
                    id=f_wlan_policy.NetworkIdentifier(
                        ssid=list(target_ssid.encode("utf-8")),
                        type=security_type.to_fidl(),
                    ),
                    credential=Credential.from_password(target_pwd).to_fidl(),
                ),
            )
            if res.err:
                raise wlan_errors.HoneydewWlanError(
                    "ClientController.SaveNetworks() NetworkConfigChangeError "
                    f"{res.err.name}"
                )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientController.SaveNetwork() error {status}"
            ) from status

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def scan_for_networks(self) -> list[str]:
        """Scans for networks.

        Returns:
            A list of network SSIDs that can be connected to.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TypeError: Return value not a list.
            RuntimeError: Client controller has not been initialized
        """
        if self._client_controller is None:
            raise RuntimeError(
                "Client controller has not been initialized; call "
                "create_client_controller() before scan_for_networks()"
            )

        client, server = Channel.create()
        iterator = f_wlan_policy.ScanResultIterator.Client(client.take())

        try:
            self._client_controller.proxy.scan_for_networks(
                iterator=server.take(),
            )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientController.ScanForNetworks() error {status}"
            ) from status

        return list(
            {
                bytes(scan_result.id.ssid).decode("utf-8")
                for res in await collect_iterator(iterator)
                for scan_result in res.response.scan_results
            }
        )

    def set_new_update_listener(self) -> None:
        """Sets the update listener stream of the facade to a new stream.

        This causes updates to be reset. Intended to be used between tests so
        that the behavior of updates in a test is independent from previous
        tests.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        if self._client_controller is None:
            # There is no running fuchsia.wlan.policy/ClientStateUpdates server.
            # Creating one is equivalent to creating a new update listener.
            self.create_client_controller()
            return

        # Replace the existing ClientStateUpdates server without giving up our
        # handle to ClientController. This is necessary since the ClientProvider
        # API is designed to only allow a single caller to make ClientController
        # calls which would impact WLAN state. If we lose our handle to the
        # ClientController, some other component on the system could take it.
        self.cancel_task(
            self._client_controller.client_state_updates_server_task
        )

        client_listener_proxy = f_wlan_policy.ClientListener.Client(
            self._fc_transport.connect_device_proxy(_CLIENT_LISTENER_PROXY)
        )

        updates: asyncio.Queue[ClientStateSummary] = asyncio.Queue()
        updates_client, updates_server = Channel.create()
        client_state_updates_server = ClientStateUpdatesImpl(
            updates_server, updates
        )
        task = self._async_adapter_loop.create_task(
            client_state_updates_server.serve()
        )

        try:
            client_listener_proxy.get_listener(
                updates=updates_client.take(),
            )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientListener.GetListener() error {status}"
            ) from status

        self._client_controller.updates = updates
        self._client_controller.client_state_updates_server_task = task

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def start_client_connections(self) -> None:
        """Enables device to initiate connections to networks.

        Either by auto-connecting to saved networks or acting on incoming calls
        triggering connections.

        See fuchsia.wlan.policy/ClientController.StartClientConnections().

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            RuntimeError: A client controller has not been created yet
        """
        if self._client_controller is None:
            raise RuntimeError(
                "Client controller has not been initialized; call "
                "create_client_controller() before start_client_connections()"
            )

        try:
            resp = (
                await self._client_controller.proxy.start_client_connections()
            )
            status = RequestStatus.from_fidl(resp.status)
            if status != RequestStatus.ACKNOWLEDGED:
                raise wlan_errors.HoneydewWlanError(
                    "ClientController.StartClientConnections() returned "
                    f"request status {status}"
                )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientController.StartClientConnections() error {status}"
            ) from status

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def stop_client_connections(self) -> None:
        """Disables device for initiating connections to networks.

        Tears down any existing connections to WLAN networks and disables
        initiation of new connections.

        See fuchsia.wlan.policy/ClientController.StopClientConnections().

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            RuntimeError: A client controller has not been created yet
        """
        if self._client_controller is None:
            raise RuntimeError(
                "Client controller has not been initialized; call "
                "create_client_controller() before stop_client_connections()"
            )

        try:
            resp = await self._client_controller.proxy.stop_client_connections()
            status = RequestStatus.from_fidl(resp.status)
            if status != RequestStatus.ACKNOWLEDGED:
                raise wlan_errors.HoneydewWlanError(
                    "ClientController.StopClientConnections() returned "
                    f"request status {status}"
                )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientController.StopClientConnections() error {status}"
            ) from status


class ClientStateUpdatesImpl(f_wlan_policy.ClientStateUpdates.Server):
    """Server to receive WLAN status changes.

    Receives updates for client connections and the associated network state
    These updates contain information about whether or not the device will
    attempt to connect to networks, saved network configuration change
    information, individual connection state information by NetworkIdentifier
    and connection attempt information.
    """

    def __init__(
        self, server: Channel, updates: asyncio.Queue[ClientStateSummary]
    ) -> None:
        super().__init__(server)
        self._updates = updates
        _LOGGER.debug("Started ClientStateUpdates server")

    async def on_client_state_update(
        self,
        request: f_wlan_policy.ClientStateUpdatesOnClientStateUpdateRequest,
    ) -> None:
        """Detected a change to the state or registered listeners.

        Args:
            request: Current summary of WLAN client state.
        """
        summary = ClientStateSummary.from_fidl(request.summary)
        _LOGGER.debug("OnClientStateUpdate called with %s", repr(summary))
        await self._updates.put(summary)
