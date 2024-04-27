#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains Abstract Base Classes for all affordances capable devices."""

import abc
from collections.abc import Callable

from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing import custom_types


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
        self, timeout: float = fuchsia_device.TIMEOUTS["OFFLINE"]
    ) -> None:
        """Wait for Fuchsia device to go offline.

        Args:
            timeout: How long in sec to wait for device to go offline.
        """

    @abc.abstractmethod
    def wait_for_online(
        self, timeout: float = fuchsia_device.TIMEOUTS["ONLINE"]
    ) -> None:
        """Wait for Fuchsia device to go online.

        Args:
            timeout: How long in sec to wait for device to go offline.
        """


class FuchsiaDeviceLogger(abc.ABC):
    """Abstract base class which contains methods for logging message to fuchsia
    device."""

    @abc.abstractmethod
    def log_message_to_device(
        self, message: str, level: custom_types.LEVEL
    ) -> None:
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.
        """
