# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Custom types for ui affordances"""

from dataclasses import dataclass


@dataclass
class Coordinate:
    x: int
    y: int


@dataclass
class Size:
    width: int
    height: int
