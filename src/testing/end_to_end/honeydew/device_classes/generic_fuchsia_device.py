#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generic fuchsia device class."""

from honeydew.device_classes import fuchsia_device_base


class GenericFuchsiaDevice(fuchsia_device_base.FuchsiaDeviceBase):
    """Generic fuchsia device class.

    This class will extend from FuchsiaDeviceBase and adds all the capabilities
    supported by Fuchsia platform irrespective of device type.
    """
