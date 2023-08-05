#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""vim3 (Fuchsia running on vim3) device class."""

from honeydew.device_classes.sl4f import fuchsia_device


class VIM3(fuchsia_device.FuchsiaDevice):
    """vim3 device class."""
