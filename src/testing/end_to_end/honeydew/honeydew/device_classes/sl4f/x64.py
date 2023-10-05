#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""x64 (Fuchsia running on Intel NUC) device class."""

from honeydew.device_classes.sl4f import fuchsia_device


class X64(fuchsia_device.FuchsiaDevice):
    """X64 device class."""
