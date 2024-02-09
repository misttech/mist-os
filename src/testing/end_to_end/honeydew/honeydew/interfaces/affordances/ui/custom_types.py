# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Custom types for ui affordances"""

from dataclasses import dataclass

# TODO(b/320551643): Remove this file once https://turquoise-internal-review.git.corp.google.com/c/vendor/google/+/806870 lands


@dataclass(frozen=True)
class Coordinate:
    x: int
    y: int


@dataclass(frozen=True)
class Size:
    width: int
    height: int
