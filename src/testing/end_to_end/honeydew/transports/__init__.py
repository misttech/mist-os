#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""HoneyDew Transports module."""

import enum


class TRANSPORT(enum.Enum):
    """Different Host-(Fuchsia)Target interaction transports supported."""
    # use SL4F for Host-(Fuchsia)Target interactions.
    SL4F = "sl4f"

    # use Fuchsia-Controller for Host-(Fuchsia)Target interactions.
    FUCHSIA_CONTROLLER = "fuchsia_controller"


DEFAULT_TRANSPORT: TRANSPORT = TRANSPORT.SL4F
