# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Screenshot affordance implementation using Fuchsia-Controller."""

from honeydew.interfaces.affordances.ui import screenshot


class Screenshot(screenshot.Screenshot):
    """Screenshot affordance implementation using Fuchsia-Controller."""

    def take(self) -> screenshot.ScreenshotImage:
        """Take a screenshot.

        Return:
            ScreenshotImage: the screenshot image.
        """
        raise NotImplementedError
