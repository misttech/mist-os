# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""WLAN affordance implementation using Fuchsia Controller."""

import asyncio
import logging

import fidl.fuchsia_location_namedplace as f_location_namedplace
import fidl.fuchsia_wlan_common as f_wlan_common
import fidl.fuchsia_wlan_device_service as f_wlan_device_service
import fidl.fuchsia_wlan_ieee80211 as f_wlan_ieee80211
import fidl.fuchsia_wlan_sme as f_wlan_sme
from fidl._client import FidlClient
from fuchsia_controller_py import Channel, ZxStatus
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod

from honeydew import errors
from honeydew.affordances.connectivity.wlan.utils import errors as wlan_errors
from honeydew.affordances.connectivity.wlan.utils.types import (
    Authentication,
    BssDescription,
    ClientStatusConnected,
    ClientStatusResponse,
    CountryCode,
    MacAddress,
    QueryIfaceResponse,
    WlanMacRole,
)
from honeydew.affordances.connectivity.wlan.wlan import wlan
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.custom_types import FidlEndpoint

# List of required FIDLs for the WLAN Fuchsia Controller affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.wlan.device.service",
    "fuchsia.wlan.phyimpl",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)

# Fuchsia Controller proxies
_DEVICE_MONITOR_PROXY = FidlEndpoint(
    "core/wlandevicemonitor", "fuchsia.wlan.device.service.DeviceMonitor"
)
_REGULATORY_REGION_CONFIGURATOR_PROXY = FidlEndpoint(
    "core/regulatory_region",
    "fuchsia.location.namedplace.RegulatoryRegionConfigurator",
)


class Wlan(AsyncAdapter, wlan.Wlan):
    """WLAN affordance implemented with Fuchsia Controller."""

    def __init__(
        self,
        device_name: str,
        ffx: ffx_transport.FFX,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
        fuchsia_device_close: affordances_capable.FuchsiaDeviceClose,
    ) -> None:
        """Create a WLAN Fuchsia Controller affordance.

        Args:
            device_name: Device name returned by `ffx target list`.
            ffx: FFX transport.
            fuchsia_controller: Fuchsia Controller transport.
            reboot_affordance: Object that implements RebootCapableDevice.
            fuchsia_device_close: Object that implements FuchsiaDeviceClose.
        """
        AsyncAdapter.__init__(self)

        self._device_name: str = device_name
        self._ffx: ffx_transport.FFX = ffx
        self._fc_transport = fuchsia_controller
        self._reboot_affordance = reboot_affordance
        self._fuchsia_device_close = fuchsia_device_close

        self.verify_supported()

        self._connect_proxy()
        self._reboot_affordance.register_for_on_device_boot(self._connect_proxy)

        self._fuchsia_device_close.register_for_on_device_close(self.close)

    def close(self) -> None:
        """Clean up async tasks.

        This is idempotent and irreversible. No other methods should be called
        after this one.
        """
        if not self.loop().is_closed():
            self.loop().stop()
            self.loop().run_forever()  # Handle pending tasks
            self.loop().close()

    def verify_supported(self) -> None:
        """Verifies that the WLAN affordance using FuchsiaController is supported by the Fuchsia
        device.

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
        self._device_monitor_proxy = f_wlan_device_service.DeviceMonitor.Client(
            self._fc_transport.connect_device_proxy(_DEVICE_MONITOR_PROXY)
        )

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def connect(
        self,
        ssid: str,
        # TODO(http://b/356234331): Remove the password field once
        # authentication is used everywhere.
        password: str | None,
        bss_desc: BssDescription,
        authentication: Authentication | None = None,
    ) -> bool:
        """Trigger connection to a network.

        Args:
            ssid: The network to connect to.
            password: The password for the network. Deprecated; use
                authentication instead.
            bss_desc: The basic service set for target network.
            authentication: Authentication to connect with.

        Returns:
            True on success otherwise false.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            NetworkInterfaceNotFoundError: No client WLAN interface found.
            TypeError: When authentication is not provided.
        """
        if authentication is None:
            raise TypeError(
                "authentication is required for the WLAN FC affordance"
            )

        iface_id = await self._get_first_sme(WlanMacRole.CLIENT)
        sme = await self._get_client_sme(iface_id)

        client, server = Channel.create()
        connect_transaction_client = f_wlan_sme.ConnectTransaction.Client(
            client.take()
        )

        req = f_wlan_sme.ConnectRequest(
            ssid=list(ssid.encode("utf-8")),
            bss_description=bss_desc.to_fidl(),
            multiple_bss_candidates=False,  # only used for metrics, selected arbitrarily
            authentication=authentication.to_fidl(),
            deprecated_scan_type=f_wlan_common.ScanType.ACTIVE,
        )

        # Run an event handler in the background before starting the connect.
        results: asyncio.Queue[f_wlan_sme.ConnectResult] = asyncio.Queue()
        event_handler = ConnectTransactionEventHandler(
            connect_transaction_client, results
        )
        event_handler_task = self.loop().create_task(event_handler.serve())

        try:
            # Initiate the connect.
            sme.connect(req=req, txn=server.take())

            # Wait for the driver to finish connecting.
            response = await asyncio.wait_for(results.get(), timeout=60)
            result = response.result
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientSme.Connect() error {status}"
            ) from status
        except TimeoutError as e:
            raise wlan_errors.HoneydewWlanError(
                f'Timed out waiting for the WLAN SME to connect to "{ssid}"'
            ) from e
        finally:
            event_handler_task.cancel()
            try:
                await event_handler_task
            except asyncio.exceptions.CancelledError:
                pass  # expected

        # Verify the connection.
        if result.code != f_wlan_ieee80211.StatusCode.SUCCESS:
            code = f_wlan_ieee80211.StatusCode(result.code)
            raise wlan_errors.HoneydewWlanError(
                f'Failed to connect to "{ssid}", received {code.name}'
                f"({code.value})"
            )
        if result.is_credential_rejected:
            raise wlan_errors.HoneydewWlanError(
                f'Failed to connect to "{ssid}", credentials were rejected.'
            )

        client_status = await self._status(sme)
        if isinstance(client_status, ClientStatusConnected):
            got_ssid = bytes(client_status.ssid).decode("utf-8")
            if got_ssid != ssid:
                raise wlan_errors.HoneydewWlanError(
                    "Connected to wrong network. "
                    f'Expected "{ssid}", got "{got_ssid}".'
                )
        else:
            raise wlan_errors.HoneydewWlanError(
                f"Expected ClientStatusConnected, got {client_status}"
            )

        return True

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def create_iface(
        self, phy_id: int, role: WlanMacRole, sta_addr: str | None = None
    ) -> int:
        """Create a new WLAN interface.

        Args:
            phy_id: The iface ID.
            role: The role of the new iface.
            sta_addr: MAC address for softAP iface.

        Returns:
            Iface id of newly created interface.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            ValueError: Invalid MAC address
        """
        if sta_addr is None:
            sta_addr = "00:00:00:00:00:00"
            _LOGGER.warning(
                "No MAC provided in args of create_iface, using %s", sta_addr
            )

        req = f_wlan_device_service.CreateIfaceRequest(
            phy_id=phy_id,
            role=role.to_fidl(),
            sta_addr=MacAddress(sta_addr).bytes(),
        )
        try:
            create_iface = await self._device_monitor_proxy.create_iface(
                req=req
            )
            if create_iface.status != ZxStatus.ZX_OK:
                raise ZxStatus(create_iface.status)
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"DeviceMonitor.CreateIface() error {status}"
            ) from status

        return create_iface.resp.iface_id

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def destroy_iface(self, iface_id: int) -> None:
        """Destroy WLAN interface by ID.

        Args:
            iface_id: The interface to destroy.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        req = f_wlan_device_service.DestroyIfaceRequest(iface_id=iface_id)
        try:
            destroy_iface = await self._device_monitor_proxy.destroy_iface(
                req=req
            )
            if destroy_iface.status != ZxStatus.ZX_OK:
                raise ZxStatus(destroy_iface.status)
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"DeviceMonitor.DestroyIface() error {status}"
            ) from status

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def disconnect(self) -> None:
        """Disconnect all client WLAN connections.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        iface_ids = await self._get_iface_id_list()

        for iface_id in iface_ids:
            info = await self._query_iface(iface_id)
            if info.role == WlanMacRole.CLIENT:
                sme = await self._get_client_sme(iface_id)
                try:
                    await sme.disconnect(
                        reason=f_wlan_sme.UserDisconnectReason.WLAN_SERVICE_UTIL_TESTING,
                    )
                except ZxStatus as status:
                    raise wlan_errors.HoneydewWlanError(
                        f"SmeClient.Disconnect() error {status}"
                    ) from status

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def get_country(self, phy_id: int) -> CountryCode:
        """Queries the currently configured country code from phy `phy_id`.

        Args:
            phy_id: A phy id that is present on the device.

        Returns:
            The currently configured country code from `phy_id`.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        try:
            get_country = await self._device_monitor_proxy.get_country(
                phy_id=phy_id
            )
            if get_country.err is not None:
                raise ZxStatus(get_country.err)
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"DeviceMonitor.GetCountry() error {status}"
            ) from status

        return CountryCode(
            bytes(get_country.response.resp.alpha2).decode("utf-8")
        )

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def get_iface_id_list(self) -> list[int]:
        """Get list of wlan iface IDs on device.

        Returns:
            A list of wlan iface IDs that are present on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        return await self._get_iface_id_list()

    async def _get_iface_id_list(self) -> list[int]:
        try:
            resp = await self._device_monitor_proxy.list_ifaces()
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"DeviceMonitor.ListIfaces() error {status}"
            ) from status
        return resp.iface_list

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def get_phy_id_list(self) -> list[int]:
        """Get list of phy ids on device.

        Returns:
            A list of phy ids that is present on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        try:
            resp = await self._device_monitor_proxy.list_phys()
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"DeviceMonitor.ListPhys() error {status}"
            ) from status
        return resp.phy_list

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def query_iface(self, iface_id: int) -> QueryIfaceResponse:
        """Retrieves interface info for given wlan iface id.

        Args:
            iface_id: The wlan interface id to get info from.

        Returns:
            QueryIfaceResponseWrapper from the SL4F server.

        Raises:
            HoneydewWlanError: DeviceMonitor.QueryIface error
        """
        return await self._query_iface(iface_id)

    async def _query_iface(self, iface_id: int) -> QueryIfaceResponse:
        try:
            query = await self._device_monitor_proxy.query_iface(
                iface_id=iface_id
            )
            if query.err is not None:
                raise query.err
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"DeviceMonitor.QueryIface() error {status}"
            ) from status
        return QueryIfaceResponse.from_fidl(query.response.resp)

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def scan_for_bss_info(self) -> dict[str, list[BssDescription]]:
        """Scans and returns BSS info.

        Returns:
            A dict mapping each seen SSID to a list of BSS Description IE
            blocks, one for each BSS observed in the network

        Raises:
            HoneydewWlanError: Error from WLAN stack
            NetworkInterfaceNotFoundError: No client WLAN interface found.
        """
        iface_id = await self._get_first_sme(WlanMacRole.CLIENT)
        client_sme = await self._get_client_sme(iface_id)

        # Perform a passive scan
        req = f_wlan_sme.ScanRequest()
        req.passive = f_wlan_sme.PassiveScanRequest()

        try:
            scan = await client_sme.scan_for_controller(req=req)
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientSme.ScanForController() error {status}"
            ) from status

        if scan.err is not None:
            raise wlan_errors.HoneydewWlanError(
                f"ClientSme.ScanForController() ScanErrorCode {scan.err}"
            )

        results: dict[str, list[BssDescription]] = {}

        for scan_result in scan.response.scan_results:
            desc = BssDescription.from_fidl(scan_result.bss_description)
            ssid = desc.ssid()
            if ssid:
                if ssid in results:
                    results[ssid].append(desc)
                else:
                    results[ssid] = [desc]
            else:
                _LOGGER.warning(
                    "Scan result does not contain SSID: %s", scan_result
                )

        return results

    async def _get_first_sme(self, role: WlanMacRole) -> int:
        """Find a WLAN interface running with the specified role.

        Args:
            role: Desired mode of the WLAN interface.

        Raises:
            NetworkInterfaceNotFoundError: No WLAN interface found running with the
                specified role.

        Returns:
            ID of the first WLAN interface found running with the specified
            role. The others are discarded.
        """
        iface_ids = await self._get_iface_id_list()
        if len(iface_ids) == 0:
            raise wlan_errors.NetworkInterfaceNotFoundError(
                "No WLAN interface found"
            )

        for iface_id in iface_ids:
            info = await self._query_iface(iface_id)
            if info.role == role:
                return iface_id

        raise wlan_errors.NetworkInterfaceNotFoundError(
            f"WLAN interface with role {role} not found"
        )

    async def _get_client_sme(
        self, iface_id: int
    ) -> f_wlan_sme.ClientSme.Client:
        """Get a handle to ClientSme for performing SME actions.

        Args:
            iface_id: The wlan interface id to connect with

        Raises:
            HoneydewWlanError: DeviceMonitor.QueryIface error

        Returns:
            Client-side handle to the fuchsia.wlan.sme.ClientSme protocol, used
            for performing driver-layer actions on the underlying WLAN hardware.
        """
        client, server = Channel.create()
        sme_client = f_wlan_sme.ClientSme.Client(client)

        try:
            await self._device_monitor_proxy.get_client_sme(
                iface_id=iface_id, sme_server=server.take()
            )
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"DeviceMonitor.GetClientSme() error {status}"
            ) from status

        return sme_client

    def set_region(self, region_code: str) -> None:
        """Set regulatory region.

        Args:
            region_code: 2-byte ASCII string.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            TypeError: Invalid region_code format
        """
        if len(region_code) != 2:
            raise TypeError(
                f'Expected region_code to be length 2, got "{region_code}"'
            )

        # TODO(http://b/323967235): Move this to its own affordance
        regulatory_region_configurator = (
            f_location_namedplace.RegulatoryRegionConfigurator.Client(
                self._fc_transport.connect_device_proxy(
                    _REGULATORY_REGION_CONFIGURATOR_PROXY
                )
            )
        )

        try:
            regulatory_region_configurator.set_region(region=region_code)
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"RegulatoryRegionConfigurator.SetRegion() error {status}"
            ) from status

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def status(self) -> ClientStatusResponse:
        """Request connection status

        Returns:
            ClientStatusResponse which can be any one of three  things:
            ClientStatusConnected, ClientStatusConnecting, ClientStatusIdle.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            NetworkInterfaceNotFoundError: No client WLAN interface found.
            TypeError: If any of the return values are not of the expected type.
        """
        iface_id = await self._get_first_sme(WlanMacRole.CLIENT)
        sme = await self._get_client_sme(iface_id)
        return await self._status(sme)

    async def _status(
        self, sme: f_wlan_sme.ClientSme.Client
    ) -> ClientStatusResponse:
        try:
            resp = await sme.status()
        except ZxStatus as status:
            raise wlan_errors.HoneydewWlanError(
                f"ClientSme.Status() error {status}"
            ) from status

        return ClientStatusResponse.from_fidl(resp.resp)


class ConnectTransactionEventHandler(
    f_wlan_sme.ConnectTransaction.EventHandler
):
    """Event handler for ClientSme.Connect()."""

    def __init__(
        self,
        client: FidlClient,
        connect_results: asyncio.Queue[f_wlan_sme.ConnectResult],
    ) -> None:
        super().__init__(client)
        self._connect_results = connect_results

    async def on_connect_result(
        self, req: f_wlan_sme.ConnectTransactionOnConnectResultRequest
    ) -> None:
        """Return the result of the initial connection request or later
        SME-initiated reconnection."""
        _LOGGER.debug(
            "ConnectTransaction.OnConnectResult() called with %s", req
        )
        await self._connect_results.put(req)

    def on_disconnect(
        self, req: f_wlan_sme.ConnectTransactionOnDisconnectRequest
    ) -> None:
        """Notify that the client has disconnected.

        If req.disconnect_info indicates that SME is attempting to reconnect by
        itself, there's not need for caller to intervene for now.
        """
        _LOGGER.debug("ConnectTransaction.OnDisconnect() called with %s", req)

    def on_roam_result(
        self, req: f_wlan_sme.ConnectTransactionOnRoamResultRequest
    ) -> None:
        """Report the result of a roam attempt."""
        _LOGGER.debug("ConnectTransaction.OnRoamResult() called with %s", req)

    def on_signal_report(
        self, req: f_wlan_sme.ConnectTransactionOnSignalReportRequest
    ) -> None:
        """Give an update of the latest signal report."""
        _LOGGER.debug("ConnectTransaction.OnSignalReport() called with %s", req)

    def on_channel_switched(
        self, req: f_wlan_sme.ConnectTransactionOnChannelSwitchedRequest
    ) -> None:
        """Give an update of the channel switching."""
        _LOGGER.debug(
            "ConnectTransaction.OnChannelSwitched() called with %s", req
        )
