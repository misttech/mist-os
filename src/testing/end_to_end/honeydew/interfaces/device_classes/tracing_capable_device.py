#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for a tracing capable device."""

import abc

from honeydew.interfaces.affordances import tracing
from honeydew.utils import properties


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
