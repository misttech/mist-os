# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Data types used by Bluetooth affordance."""

import enum


class BluetoothConnectionType(enum.Enum):
    """Transport type of Bluetooth pair and/or connections."""

    CLASSIC = 1
    LOW_ENERGY = 2


class BluetoothAcceptPairing(enum.StrEnum):
    """Pairing modes for Bluetooth Accept Pairing."""

    DEFAULT_INPUT_MODE = "NONE"
    DEFAULT_OUTPUT_MODE = "NONE"


class BluetoothAvrcpCommand(enum.StrEnum):
    """Commands that the AVRCP service can send."""

    PLAY = "Play"
    PAUSE = "Pause"
