#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for a component capable device."""

import abc

from honeydew.interfaces.affordances import component
from honeydew.utils import properties


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
