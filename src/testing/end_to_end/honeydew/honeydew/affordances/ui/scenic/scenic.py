# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Abstract base class for Scenic affordance."""

import abc

from honeydew.affordances import affordance


class Scenic(affordance.Affordance):
    """Abstract base class for Scenic affordance."""

    @abc.abstractmethod
    def renderer(self) -> str:
        """Get type of the scenic renderer.

        CPU renderer only supports limited features, tests maybe failed on CPU render.

        TODO(b/406505382): cpu renderer does not showing flatland-rainbow, because the image size is
        not equal to the view size. In vulkan renderer, vulkan will compute the transition color.

        Raise:
            ScenicError: if failed to get renderer type.
        """
