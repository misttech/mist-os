# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""scenic affordance implementation using ffx."""

import re

from honeydew import errors
from honeydew.affordances.ui.scenic import errors as scenic_errors
from honeydew.affordances.ui.scenic import scenic
from honeydew.transports.ffx import errors as ffx_errors
from honeydew.transports.ffx import ffx as ffx_transport


class ScenicUsingFfx(scenic.Scenic):
    """Scenic affordance implementation using ffx.

    Args:
        ffx: ffx_transport.FFX.
    """

    def __init__(self, ffx: ffx_transport.FFX) -> None:
        self._ffx: ffx_transport.FFX = ffx

        self.verify_supported()

    def verify_supported(self) -> None:
        """verify if the device support scenic.

        Raises:
            errors.NotSupportedError: if scenic is not supported.
        """
        try:
            self._ffx.run(["component", "show", "core/ui/scenic"])
        except ffx_errors.FfxCommandError as err:
            raise errors.NotSupportedError(err)

    def renderer(self) -> str:
        """Get type of the scenic renderer.

        CPU renderer only supports limited features, tests maybe failed on CPU render.

        TODO(b/406505382): cpu renderer does not showing flatland-rainbow, because the image size is
        not equal to the view size. In vulkan renderer, vulkan will compute the transition color.

        Raises:
            ScenicError: if failed to get renderer type.
        """

        try:
            res = self._ffx.run(["component", "show", "core/ui/scenic"])
            lines = res.splitlines()
            for line in lines:
                # find a line "renderer -> String("vulkan")"
                if line.find("renderer") != -1:
                    match = re.search(r'String\("(\w+)"\)', line)
                    if match:
                        return match.group(1)

            raise scenic_errors.ScenicError("can not find renderer type.")

        except ffx_errors.FfxCommandError as err:
            raise scenic_errors.ScenicError(err)
