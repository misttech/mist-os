#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains Abstract Base Classes for all affordances capable devices."""

import abc
from typing import Callable

from honeydew.interfaces.affordances import session
from honeydew.interfaces.affordances import tracing
from honeydew.interfaces.affordances.bluetooth import bluetooth_gap
from honeydew.interfaces.affordances.ui import screenshot
from honeydew.interfaces.affordances.ui import user_input
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


class ScreenshotCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    screenshot affordance."""

    @properties.Affordance
    @abc.abstractmethod
    def screenshot(self) -> screenshot.Screenshot:
        """Returns a screenshot affordance object.

        Returns:
            screenshot.Screenshot object
        """


class SessionCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    session affordance."""

    @properties.Affordance
    @abc.abstractmethod
    def session(self) -> session.Session:
        """Returns a session affordance object.

        Returns:
            session.Session object
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


class UserInputCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    user input affordance."""

    @properties.Affordance
    @abc.abstractmethod
    def user_input(self) -> user_input.UserInput:
        """Returns a user_input affordance object.

        Returns:
            user_input.UserInput object
        """
