# mypy: ignore-errors
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth LE affordance implementation using Fuchsia Controller."""

import asyncio
import logging
import uuid
from typing import Any

import fidl.fuchsia_bluetooth as f_bt
import fidl.fuchsia_bluetooth_gatt2 as f_gatt_controller
import fidl.fuchsia_bluetooth_le as f_ble_controller
import fuchsia_controller_py as fc
from fidl import StopServer
from fuchsia_controller_py import Channel

from honeydew.affordances.connectivity.bluetooth.bluetooth_common import (
    bluetooth_common_using_fc,
)
from honeydew.affordances.connectivity.bluetooth.le import le
from honeydew.affordances.connectivity.bluetooth.utils import (
    errors as bt_errors,
)
from honeydew.affordances.connectivity.bluetooth.utils import types as bt_types
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import fuchsia_controller as fc_transport
from honeydew.typing import custom_types

_LOGGER: logging.Logger = logging.getLogger(__name__)


_FC_PROXIES: dict[str, custom_types.FidlEndpoint] = {
    "BluetoothLEPeripheral": custom_types.FidlEndpoint(
        "core/bluetooth-core", "fuchsia.bluetooth.le.Peripheral"
    ),
    "BluetoothLECentral": custom_types.FidlEndpoint(
        "core/bluetooth-core", "fuchsia.bluetooth.le.Central"
    ),
    "BluetoothLEScanWatcher": custom_types.FidlEndpoint(
        "core/bluetooth-core", "fuchsia.bluetooth.le.ScanResultWatcher"
    ),
    "BluetoothGattServer": custom_types.FidlEndpoint(
        "core/bluetooth-core", "fuchsia.bluetooth.gatt2.Server"
    ),
}

ASYNC_OP_TIMEOUT: int = 10


class AdvertisedPeripheralImpl(f_ble_controller.AdvertisedPeripheral.Server):
    def on_connected(
        self, request: f_ble_controller.AdvertisedPeripheralOnConnectedRequest
    ) -> None:
        _LOGGER.info(
            "Advertised Peripheral Connected with peer: %s",
            request.peer.id.value,
        )
        self._peripheral_connection = request.connection
        raise StopServer


class LEUsingFc(le.LE, bluetooth_common_using_fc.BluetoothCommonUsingFc):
    """BluetoothLE Common affordance implementation using Fuchsia Controller.

    Args:
        device_name: Device name returned by `ffx target list`.
        fuchsia_controller: FC transport.
    """

    def __init__(
        self,
        device_name: str,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
    ) -> None:
        super().__init__(
            device_name=device_name,
            fuchsia_controller=fuchsia_controller,
            reboot_affordance=reboot_affordance,
        )
        self.service_info: dict[int, dict[str, Any]]
        self._peripheral_advertisement_server: asyncio.Task[None] | None = None
        self._name: str = device_name
        self._connection_client: fc.Channel | None = None
        self._gatt_client: fc.Channel | None = None
        self._remote_service_client: fc.Channel | None = None
        self._peripheral_connection: fc.Channel | None = None
        self._fc_transport: fc_transport.FuchsiaController = fuchsia_controller
        self._reboot_affordance: affordances_capable.RebootCapableDevice = (
            reboot_affordance
        )
        self._peripheral_controller_proxy: (
            f_ble_controller.Peripheral.Client | None
        ) = None
        self._central_controller_proxy: (
            f_ble_controller.Central.Client | None
        ) = None
        self._gatt_server_proxy: f_gatt_controller.Server.Client | None = None
        self._le_session_initialized = False
        self._reboot_affordance.register_for_on_device_boot(fn=self.init_le_sys)
        self.init_le_sys()

    def reset_state(self) -> None:
        """Reset the internal state tracking variables to correspond to an inactive BLE State."""
        self._peripheral_controller_proxy = None
        self._central_controller_proxy = None
        self._gatt_server_proxy = None
        self._remote_service_client = None
        if self._peripheral_advertisement_server is not None:
            _LOGGER.debug(
                "Cancelling Peripheral Advertisement Server and setting to None"
            )
            self._peripheral_advertisement_server.cancel()
            self._peripheral_advertisement_server = None
        self._peripheral_connection = None
        self._le_session_initialized = False
        super().reset_state()

    def init_le_sys(self) -> None:
        """Initializes BLE stack.

        Note: This method is called automatically:
            1. During this class initialization
            2. After the device reboot

        Raises:
            BluetoothStateError: On failure.
        """
        if self._le_session_initialized:
            raise bt_errors.BluetoothStateError(
                f"Bluetooth session is already initialized on {self._device_name}. Can be "
                "initialized only once."
            )

        assert self._peripheral_controller_proxy is None
        self._peripheral_controller_proxy = f_ble_controller.Peripheral.Client(
            self._fc_transport.connect_device_proxy(
                _FC_PROXIES["BluetoothLEPeripheral"]
            )
        )

        assert self._central_controller_proxy is None
        self._central_controller_proxy = f_ble_controller.Central.Client(
            self._fc_transport.connect_device_proxy(
                _FC_PROXIES["BluetoothLECentral"]
            )
        )
        assert self._gatt_server_proxy is None
        self._gatt_server_proxy = f_gatt_controller.Server.Client(
            self._fc_transport.connect_device_proxy(
                _FC_PROXIES["BluetoothGattServer"]
            )
        )
        self._le_session_initialized = True
        self.known_le_devices: dict[str, Any] = dict()
        self.service_info: dict[int, dict[str, Any]] = dict()
        self._uuid = f_bt.Uuid(value=self._generate_random_bluetooth_uuid())

    def stop_advertise(self) -> None:
        """Stop advertising the peripheral."""
        self._peripheral_advertisement_server = None

    def scan(self) -> dict[str, bool | int | str]:
        """Perform an LE scan on central device.

        Returns:
            A dict of all known LE remote devices.
        """
        try:
            return self.loop.run_until_complete(
                asyncio.wait_for(
                    self._scan(),
                    ASYNC_OP_TIMEOUT,
                )
            )
        except TimeoutError:
            _LOGGER.info(
                "No updates on % from watcher.watch(), returning cached peers.",
                self._device_name,
            )
            return self.known_le_devices

    async def _scan(self) -> dict[str, Any]:
        """Async LE scan function on central device.
        TaskGroup creates first task that scans for peripheral devices, then a second task that waits
        and watches the scan task for new information. Once, the watcher completes, close the channel
        to the watcher, and await the first task.

        Returns:
            A dict of all known LE remote devices.
        """
        (central_client, central_server) = Channel.create()
        watcher = f_ble_controller.ScanResultWatcher.Client(central_client)
        filter_options = f_ble_controller.Filter()
        scan_options = f_ble_controller.ScanOptions(filters=[filter_options])
        assert self._central_controller_proxy is not None
        async with asyncio.TaskGroup() as tg:
            task1 = tg.create_task(
                self._central_controller_proxy.scan(
                    options=scan_options, result_watcher=central_server.take()
                )
            )
            res = await watcher.watch()
            central_client.close()
            await task1
        for peer in res.updated:
            self.known_le_devices[peer.id.value] = {
                "name": peer.name,
                "id": peer.id,
                "bonded": peer.bonded,
                "connectable": peer.connectable,
            }
        return self.known_le_devices

    def connect(self, identifier: int) -> None:
        """Initiate connection from the central device to peripheral.

        Args:
            identifier: the identifier of target remote device.

        Raises:
            BluetoothError: If the peripheral is not initialized.
        """
        peer_id = f_bt.PeerId(value=identifier)
        (conn_client, conn_server) = Channel.create()
        self._connection_client = f_ble_controller.Connection.Client(
            conn_client.take()
        )
        connection_options = f_ble_controller.ConnectionOptions(
            bondable_mode=True
        )
        try:
            assert self._central_controller_proxy is not None
            self._central_controller_proxy.connect(
                id=peer_id,
                options=connection_options,
                handle=conn_server.take(),
            )
            # TODO: b/342432248 - Reduce sleep values to minimum stables values
            self.loop.run_until_complete(asyncio.sleep(5))
        except Exception as e:  # pylint: disable=broad-except
            raise bt_errors.BluetoothError(
                f"Failed to complete BLE connect FIDL on {self._device_name}."
            ) from e

    def advertise(
        self, appearance: bt_types.BluetoothLEAppearance, name: str
    ) -> None:
        """Advertise the peripheral.

        Args:
            appearance: Peripheral device appearance.
            name: Peripheral device name.
        """
        try:
            self.loop.run_until_complete(self._advertise(appearance, name))
        except Exception as e:
            raise bt_errors.BluetoothError(
                f"Failed to complete BLE advertise FIDL call on {self._device_name}."
            ) from e

    async def _advertise(
        self, appearance: bt_types.BluetoothLEAppearance, name: str
    ) -> None:
        """Async function to advertise the peripheral.

        Args:
            appearance: Peripheral device appearance.
            name: Peripheral device name.
        """
        connection_options = f_ble_controller.ConnectionOptions(
            bondable_mode=True
        )
        advertising_data = f_ble_controller.AdvertisingData(
            name=name, appearance=appearance, service_uuids=[self._uuid]
        )
        params = f_ble_controller.AdvertisingParameters(
            data=advertising_data, connection_options=connection_options
        )
        (client, server) = Channel.create()
        advertised_server = AdvertisedPeripheralImpl(server)
        self._peripheral_advertisement_server = (
            asyncio.get_running_loop().create_task(advertised_server.serve())
        )
        assert self._peripheral_controller_proxy is not None
        self._peripheral_controller_proxy.advertise(
            parameters=params, advertised_peripheral=client.take()
        )

    def run_advertise_connection(self) -> None:
        """Function to run Advertised Peripheral server calls"""
        if self._peripheral_advertisement_server is None:
            raise bt_errors.BluetoothError(
                "No Peripheral Advertisement server active on "
                f"device: {self._device_name}"
            )
        try:
            self.loop.run_until_complete(
                asyncio.wait_for(
                    self._peripheral_advertisement_server, ASYNC_OP_TIMEOUT
                )
            )
        except Exception as e:
            raise bt_errors.BluetoothError(
                f"Failed to complete Peripheral connection calls on {self._device_name}."
            ) from e

    def _generate_random_bluetooth_uuid(self) -> list[int]:
        """Generates a random Bluetooth UUID in its 128-bit canonical form,
        then converts the bytes into little-endian order. Finally, convert
        to a list in big-endian order.

        Returns:
            list: A list of 16 integers representing the UUID in big-endian byte order.
        """

        random_uuid = uuid.uuid4()
        uuid_bytes = random_uuid.bytes_le
        # Convert to a list of integers in big-endian order
        uuid_list = list(uuid_bytes)
        return uuid_list

    def publish_service(self) -> f_bt.Uuid:
        """Publish the Gatt service from the peripheral.

        Returns:
            The UUID of the service.

        Raises:
            NotImplementedError
        """
        raise NotImplementedError

    def request_gatt_client(self) -> None:
        """Request the Gatt Client.

        Raises:
            BluetoothError: If the peripheral fails to request the Gatt client.
        """
        try:
            assert (
                self._connection_client is not None
            )  # the central connection handle should request Gatt client
            (client, server) = Channel.create()
            client = f_gatt_controller.Client.Client(client)
            self._connection_client.request_gatt_client(
                client=server.take()
            )  # bind server end of gatt2 client
            self._gatt_client = client
        except Exception as e:
            raise bt_errors.BluetoothError(
                f"Failed to complete Request Gatt client FIDL call on {self._device_name}."
            ) from e

    def list_gatt_services(self) -> dict[int, dict[str, Any]]:
        """List the Gatt Services found on the connected peripheral.

        Returns:
            The list of Gatt Services of the connected peripheral.

        Raises:
            BluetoothError: If the device fails to complete the FIDL request.
        """
        try:
            assert (
                self._gatt_client is not None
            )  # the gatt client should not be none
            res = self.loop.run_until_complete(
                asyncio.wait_for(self._gatt_client.watch_services(uuids=[]), 10)
            )
        except TimeoutError:
            _LOGGER.info(
                "No updates on {self._device_name} from watch_services(), returning cache."
            )
            return self.service_info
        for service in res.updated:
            self.service_info[service.handle.value] = {
                "kind": service.kind,
                "type": service.type,
                "characteristics": service.characteristics,
                "includes": service.includes,
            }
        for service in res.removed:
            del self.service_info[service]
        return self.service_info

    def connect_to_service(self, handle: int) -> None:
        """Connect to an available GATT service on the peripheral device.

        Args:
            handle: The handle of the service.

        Raises:
            BluetoothError: If the central device fails to connect to Gatt service.
        """
        try:
            assert self._gatt_client is not None
            service_handle = f_gatt_controller.ServiceHandle(value=handle)
            (client, server) = Channel.create()
            self._gatt_client.connect_to_service(
                handle=service_handle, service=server.take()
            )
            client = f_gatt_controller.RemoteService.Client(client)
            self._remote_service_client = client
        except Exception as e:
            raise bt_errors.BluetoothError(
                f"Failed to complete connect_to_service FIDL call on {self._device_name}."
            ) from e

    def discover_characteristics(
        self,
    ) -> dict[int, dict[str, int | list[int] | None]]:
        """Discover characteristics of a connected Gatt Service.

        Returns:
            The available characteristics of a connected Gatt Service.
        """
        try:
            assert self._remote_service_client is not None
            res = self.loop.run_until_complete(
                asyncio.wait_for(
                    self._remote_service_client.discover_characteristics(), 10
                )
            )
        except Exception as e:
            raise bt_errors.BluetoothError(
                f"Failed to complete discover_characteristics FIDL call on {self._device_name}."
            ) from e
        remote_response = {}
        for characteristic in res.characteristics:
            remote_response[characteristic.handle.value] = {
                "handle": characteristic.handle.value,
                "type": characteristic.type.value,
                "properties": characteristic.properties,
                "permissions": characteristic.permissions,
                "descriptors": characteristic.descriptors,
            }
        return remote_response

    def read_characteristic(
        self, handle: int
    ) -> dict[str, int | list[int] | None | bool]:
        """Read characteristic of the Gatt service.

        Args:
            handle: The handle of the service.

        Returns:
            A characteristic of the Gatt service and its properties

        Raises:
            BluetoothError: If the peripheral fails to read the characteristic.
        """
        try:
            assert (
                self._remote_service_client is not None
            )  # we must have a connected client to make a remote service
            service_handle = f_gatt_controller.ServiceHandle(value=handle)
            read_options = f_gatt_controller.ReadOptions()
            read_options.short_read = f_gatt_controller.ShortReadOptions()
            res = self.loop.run_until_complete(
                asyncio.wait_for(
                    self._remote_service_client.read_characteristic(
                        handle=service_handle, options=read_options
                    ),
                    10,
                )
            )
        except Exception as e:
            raise bt_errors.BluetoothError(
                f"Failed to complete read_characteristics FIDL call on {self._device_name}."
            ) from e
        characteristic = res.response.value
        char_response = {
            "handle": characteristic.handle.value,
            "value": characteristic.value,
            "truncated": characteristic.maybe_truncated,
        }
        return char_response
