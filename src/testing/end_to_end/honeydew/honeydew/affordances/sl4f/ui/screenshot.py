# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Screenshot affordance implementation using SL4F."""

import base64
from typing import Any, Dict

from honeydew import errors
from honeydew.interfaces.affordances.ui import custom_types
from honeydew.interfaces.affordances.ui import screenshot
from honeydew.transports import sl4f as sl4f_transport

_SL4F_METHODS: Dict[str, str] = {
    "Take": "scenic_facade.TakeScreenshot",
}


class Screenshot(screenshot.Screenshot):
    """Screenshot affordance implementation using SL4F.

    Args:
        sl4f: SL4F transport.
    """

    def __init__(self, sl4f: sl4f_transport.SL4F) -> None:
        self._sl4f: sl4f_transport.SL4F = sl4f

    def take(self) -> screenshot.ScreenshotImage:
        """Take a screenshot.

        Return:
            ScreenshotImage: the screenshot image.

        Raise:
            Sl4fError: sl4f responses in unexpected format.
        """
        resp: Dict[str, Any] = self._sl4f.run(method=_SL4F_METHODS["Take"])

        try:
            width = resp["result"]["info"]["width"]
            height = resp["result"]["info"]["height"]
            img_bytes = resp["result"]["data"].encode("utf-8")
        except KeyError as err:
            raise errors.Sl4fError(err)

        data = base64.decodebytes(img_bytes)
        return screenshot.ScreenshotImage(
            size=custom_types.Size(width=width, height=height), data=data)
