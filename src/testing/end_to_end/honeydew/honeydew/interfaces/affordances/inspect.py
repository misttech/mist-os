# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for an Inspect affordance which contains APIs to
query component nodes exposed via the Inspect API."""

import abc

import fuchsia_inspect


class Inspect(abc.ABC):
    """Abstract base class for an Inspect affordance which contains APIs to
    query component nodes exposed via the Inspect API."""

    @abc.abstractmethod
    def get_data(
        self,
        selectors: list[str] | None = None,
        monikers: list[str] | None = None,
    ) -> fuchsia_inspect.InspectDataCollection:
        """Return the inspect data associated with the given selectors and
        monikers.

        Args:
            selectors: selectors to be queried.
            monikers: component monikers.

        Note: If both `selectors` and `monikers` lists are empty, inspect data
        for the whole system will be returned.

        Returns:
            Inspect data collection

        Raises:
            InspectError: Failed to return inspect data.
        """
