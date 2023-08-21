# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Screenshot affordance."""

import abc
from dataclasses import dataclass

from honeydew.interfaces.affordances.ui import custom_types


@dataclass
class ScreenshotImage:
    """Image from screenshot and the size of image

    The format is 32bit BGRA pixels in sRGB color space.
    """

    size: custom_types.Size
    data: bytes


class Screenshot(abc.ABC):
    """Abstract base class for Screenshot affordance."""

    @abc.abstractmethod
    def take(self) -> ScreenshotImage:
        """Take a screenshot.

        Return:
            ScreenshotImage: the screenshot image.
        """
