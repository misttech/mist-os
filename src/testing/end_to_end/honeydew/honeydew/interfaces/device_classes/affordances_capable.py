#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains Abstract Base Classes for all affordances capable devices."""

import abc
from typing import Callable

from honeydew.interfaces.affordances import component
from honeydew.interfaces.affordances import tracing
from honeydew.interfaces.affordances.bluetooth import bluetooth_gap
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.utils import properties


class BluetoothGapCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    Bluetooth affordance."""

    @properties.Affordance
    @abc.abstractmethod
    def bluetooth_gap(self) -> bluetooth_gap.BluetoothGap:
        """Returns a BluetoothGap affordance object.

        Returns:
            bluetooth_gap.BluetoothGap object
        """


class ComponentCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    Component affordance."""

    @properties.Affordance
    @abc.abstractmethod
    def component(self) -> component.Component:
        """Returns a component affordance object.

        Returns:
            component.Component object
        """


class RebootCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    reboot operation."""

    @abc.abstractmethod
    def reboot(self) -> None:
        """Soft reboot the device."""

    @abc.abstractmethod
    def on_device_boot(self) -> None:
        """Take actions after the device is rebooted."""

    @abc.abstractmethod
    def register_for_on_device_boot(self, fn: Callable[[], None]) -> None:
        """Register a function that will be called in on_device_boot."""

    @abc.abstractmethod
    def wait_for_offline(
            self, timeout: float = fuchsia_device.TIMEOUTS["OFFLINE"]) -> None:
        """Wait for Fuchsia device to go offline.

        Args:
            timeout: How long in sec to wait for device to go offline.
        """

    @abc.abstractmethod
    def wait_for_online(
            self, timeout: float = fuchsia_device.TIMEOUTS["ONLINE"]) -> None:
        """Wait for Fuchsia device to go online.

        Args:
            timeout: How long in sec to wait for device to go offline.
        """


class TracingCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    Tracing affordance."""

    @properties.Affordance
    @abc.abstractmethod
    def tracing(self) -> tracing.Tracing:
        """Returns a tracing affordance object.

        Returns:
            tracing.Tracing object
        """
