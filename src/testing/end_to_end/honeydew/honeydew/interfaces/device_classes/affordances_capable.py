#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains Abstract Base Classes for all affordances capable devices."""

import abc

from honeydew.interfaces.affordances.bluetooth import bluetooth_gap
from honeydew.interfaces.affordances import component
from honeydew.interfaces.affordances import tracing
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
