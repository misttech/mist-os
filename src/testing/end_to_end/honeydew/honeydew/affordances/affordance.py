# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Honeydew affordance."""

import abc


class Affordance(abc.ABC):
    """Abstract base class for Honeydew affordance.

    Every Honeydew affordance contract should inherit from this class and thus required to implement
    the methods defined in this class.
    """

    @abc.abstractmethod
    def verify_supported(self) -> None:
        """Verifies that affordance implementation is supported by the Fuchsia device.

        This method should be called in every affordance implementation's `__init__()` so that if an
        affordance is used on a Fuchsia device that does not support it, it will raise
        NotSupportedError.

        Raises:
            NotSupportedError: If affordance is not supported.
        """
