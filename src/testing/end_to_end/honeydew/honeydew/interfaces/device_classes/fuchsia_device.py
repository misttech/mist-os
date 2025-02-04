# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Fuchsia device."""

import abc
from collections.abc import Callable

from honeydew.affordances.connectivity.bluetooth.avrcp import avrcp
from honeydew.affordances.connectivity.bluetooth.gap import gap
from honeydew.affordances.connectivity.bluetooth.le import le
from honeydew.affordances.connectivity.netstack import netstack
from honeydew.affordances.connectivity.wlan.wlan import wlan
from honeydew.affordances.connectivity.wlan.wlan_policy import wlan_policy
from honeydew.affordances.connectivity.wlan.wlan_policy_ap import wlan_policy_ap
from honeydew.affordances.power.system_power_state_controller import (
    system_power_state_controller,
)
from honeydew.affordances.session import session
from honeydew.affordances.ui.screenshot import screenshot
from honeydew.affordances.ui.user_input import user_input
from honeydew.interfaces.affordances import inspect, location, rtc, tracing
from honeydew.interfaces.auxiliary_devices import (
    power_switch as power_switch_interface,
)
from honeydew.interfaces.transports import fastboot as fastboot_transport
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import (
    fuchsia_controller as fuchsia_controller_transport,
)
from honeydew.interfaces.transports import serial as serial_transport
from honeydew.interfaces.transports import sl4f as sl4f_transport
from honeydew.typing import custom_types
from honeydew.utils import properties


class FuchsiaDevice(abc.ABC):
    """Abstract base class for Fuchsia device.

    This class contains abstract methods that are supported by every device
    running Fuchsia irrespective of the device type.
    """

    # List all the persistent properties
    @properties.PersistentProperty
    @abc.abstractmethod
    def board(self) -> str:
        """Returns the board value of the device.

        Returns:
            board value of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def device_name(self) -> str:
        """Returns the name of the device.

        Returns:
            Name of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def manufacturer(self) -> str:
        """Returns the manufacturer of the device.

        Returns:
            Manufacturer of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def model(self) -> str:
        """Returns the model of the device.

        Returns:
            Model of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def product(self) -> str:
        """Returns the product value of the device.

        Returns:
            product value of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def product_name(self) -> str:
        """Returns the product name of the device.

        Returns:
            Product name of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of the device.
        """

    # List all the dynamic properties
    @properties.DynamicProperty
    @abc.abstractmethod
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of the device.
        """

    # List all the transports
    @properties.Transport
    @abc.abstractmethod
    def fastboot(self) -> fastboot_transport.Fastboot:
        """Returns the Fastboot transport object.

        Returns:
            Fastboot object.

        Raises:
            errors.FuchsiaDeviceError: Failed to instantiate.
        """

    @properties.Transport
    @abc.abstractmethod
    def ffx(self) -> ffx_transport.FFX:
        """Returns the FFX transport object.

        Returns:
            FFX object.

        Raises:
            errors.FfxCommandError: Failed to instantiate.
        """

    @properties.Transport
    @abc.abstractmethod
    def fuchsia_controller(
        self,
    ) -> fuchsia_controller_transport.FuchsiaController:
        """Returns the Fuchsia-Controller transport object.

        Returns:
            Fuchsia-Controller transport object.

        Raises:
            errors.FuchsiaControllerError: Failed to instantiate.
        """

    @properties.Transport
    @abc.abstractmethod
    def serial(self) -> serial_transport.Serial:
        """Returns the Serial transport object.

        Returns:
            Serial transport object.
        """

    @properties.Transport
    @abc.abstractmethod
    def sl4f(self) -> sl4f_transport.SL4F:
        """Returns the SL4F transport object.

        Returns:
            SL4F object.

        Raises:
            errors.Sl4fError: Failed to instantiate.
        """

    # List all the affordances
    @properties.Affordance
    @abc.abstractmethod
    def bluetooth_avrcp(self) -> avrcp.Avrcp:
        """Returns a Bluetooth Avrcp affordance object.

        Returns:
            Bluetooth Avrcp object
        """

    @properties.Affordance
    @abc.abstractmethod
    def bluetooth_gap(self) -> gap.Gap:
        """Returns a Bluetooth Gap affordance object.

        Returns:
            Bluetooth Gap object
        """

    @properties.Affordance
    @abc.abstractmethod
    def bluetooth_le(self) -> le.LE:
        """Returns a Bluetooth LE affordance object.

        Returns:
            Bluetooth LE object
        """

    @properties.Affordance
    @abc.abstractmethod
    def inspect(self) -> inspect.Inspect:
        """Returns a inspect affordance object.

        Returns:
            inspect.Inspect object
        """

    @properties.Affordance
    @abc.abstractmethod
    def rtc(self) -> rtc.Rtc:
        """Returns an RTC affordance object.

        Returns:
            rtc.Rtc object
        """

    @properties.Affordance
    @abc.abstractmethod
    def screenshot(self) -> screenshot.Screenshot:
        """Returns a screenshot affordance object.

        Returns:
            screenshot.Screenshot object
        """

    @properties.Affordance
    @abc.abstractmethod
    def session(self) -> session.Session:
        """Returns a session affordance object.

        Returns:
            session.Session object
        """

    @properties.Affordance
    @abc.abstractmethod
    def system_power_state_controller(
        self,
    ) -> system_power_state_controller.SystemPowerStateController:
        """Returns a SystemPowerStateController affordance object.

        Returns:
            system_power_state_controller.SystemPowerStateController object

        Raises:
            errors.NotSupportedError: If Fuchsia device does not support Starnix
        """

    @properties.Affordance
    @abc.abstractmethod
    def tracing(self) -> tracing.Tracing:
        """Returns a tracing affordance object.

        Returns:
            tracing.Tracing object
        """

    @properties.Affordance
    @abc.abstractmethod
    def user_input(self) -> user_input.UserInput:
        """Returns a user_input affordance object.

        Returns:
            user_input.UserInput object
        """

    @properties.Affordance
    @abc.abstractmethod
    def wlan_policy(self) -> wlan_policy.WlanPolicy:
        """Returns a WlanPolicy affordance object.

        Returns:
            wlan_policy.WlanPolicy object
        """

    @properties.Affordance
    @abc.abstractmethod
    def wlan_policy_ap(self) -> wlan_policy_ap.WlanPolicyAp:
        """Returns a WlanPolicyAp affordance object.

        Returns:
            wlan_policy_ap.WlanPolicyAp object
        """

    @properties.Affordance
    @abc.abstractmethod
    def wlan(self) -> wlan.Wlan:
        """Returns a Wlan affordance object.

        Returns:
            wlan.Wlan object
        """

    @properties.Affordance
    @abc.abstractmethod
    def netstack(self) -> netstack.Netstack:
        """Returns a netstack affordance object.

        Returns:
            netstack.Netstack object
        """

    @properties.Affordance
    @abc.abstractmethod
    def location(self) -> location.Location:
        """Returns a Location affordance object.

        Returns:
            location.Location object
        """

    # List all the public methods
    @abc.abstractmethod
    def close(self) -> None:
        """Clean up method."""

    @abc.abstractmethod
    def health_check(self) -> None:
        """Ensure device is healthy.

        Raises:
            errors.HealthCheckError
        """

    @abc.abstractmethod
    def log_message_to_device(
        self, message: str, level: custom_types.LEVEL
    ) -> None:
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.
        """

    @abc.abstractmethod
    def on_device_boot(self) -> None:
        """Take actions after the device is rebooted."""

    @abc.abstractmethod
    def power_cycle(
        self,
        power_switch: power_switch_interface.PowerSwitch,
        outlet: int | None,
    ) -> None:
        """Power cycle (power off, wait for delay, power on) the device.

        Args:
            power_switch: Implementation of PowerSwitch interface.
            outlet (int): If required by power switch hardware, outlet on
                power switch hardware where this fuchsia device is connected.
        """

    @abc.abstractmethod
    def reboot(self) -> None:
        """Soft reboot the device."""

    @abc.abstractmethod
    def register_for_on_device_boot(self, fn: Callable[[], None]) -> None:
        """Register a function that will be called in `on_device_boot()`.

        Args:
            fn: Function that need to be called after FuchsiaDevice boot up.
        """

    @abc.abstractmethod
    def register_for_on_device_close(self, fn: Callable[[], None]) -> None:
        """Register a function that will be called during device clean up in `close()`.

        Args:
            fn: Function that need to be called during FuchsiaDevice cleanup.
        """

    @abc.abstractmethod
    def snapshot(self, directory: str, snapshot_file: str | None = None) -> str:
        """Captures the snapshot of the device.

        Args:
            directory: Absolute path on the host where snapshot file need
                to be saved.

            snapshot_file: Name of the file to be used to save snapshot file.
                If not provided, API will create a name using
                "Snapshot_{device_name}_{'%Y-%m-%d-%I-%M-%S-%p'}" format.

        Returns:
            Absolute path of the snapshot file.
        """

    @abc.abstractmethod
    def wait_for_offline(self) -> None:
        """Wait for Fuchsia device to go offline."""

    @abc.abstractmethod
    def wait_for_online(self) -> None:
        """Wait for Fuchsia device to go online."""
