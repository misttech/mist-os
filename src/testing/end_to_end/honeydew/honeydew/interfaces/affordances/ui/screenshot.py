# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Screenshot affordance."""

import abc

from honeydew.typing.screenshot_image import ScreenshotImage


class Screenshot(abc.ABC):
    """Abstract base class for Screenshot affordance."""

    @abc.abstractmethod
    def take(self) -> ScreenshotImage:
        """Take a screenshot.

        Return:
            ScreenshotImage: the screenshot image.
        """
