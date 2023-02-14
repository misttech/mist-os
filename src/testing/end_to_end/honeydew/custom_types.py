#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Custom data types."""

from enum import Enum, auto


class LEVEL(Enum):
    """Logging level that need to specified to log a message onto device"""
    INFO = auto()
    WARNING = auto()
    ERROR = auto()
