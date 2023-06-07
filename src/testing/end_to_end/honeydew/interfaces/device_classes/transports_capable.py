#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains Abstract Base Classes for all transports capable devices."""

import abc

from honeydew.transports import ffx as ffx_transport
from honeydew.transports import sl4f as sl4f_transport
from honeydew.transports import ssh as ssh_transport
from honeydew.utils import properties


class FFXCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    FFX transport."""

    @properties.Transport
    @abc.abstractmethod
    def ffx(self) -> ffx_transport.FFX:
        """Returns a FFX transport object.

        Returns:
            ffx_transport.FFX object
        """


class SL4FCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    SL4F transport."""

    @properties.Transport
    @abc.abstractmethod
    def sl4f(self) -> sl4f_transport.SL4F:
        """Returns a SL4F transport object.

        Returns:
            sl4f_transport.SL4F object
        """


class SSHCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    SSH transport."""

    @properties.Transport
    @abc.abstractmethod
    def ssh(self) -> ssh_transport.SSH:
        """Returns a SSH transport object.

        Returns:
            ssh_transport.SSH object
        """
