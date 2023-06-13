#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utility module for different type of property decorators in HoneyDew."""

import functools
from typing import Any, Callable, Optional


class DynamicProperty(property):
    """A property that is dynamic and involves a device query to return."""

    def __init__(
            self,
            fget: Callable[[Any], Any],
            fset: Optional[Callable[[Any, Any], None]] = None,
            fdel: Optional[Callable[[Any], None]] = None,
            doc: Optional[str] = None) -> None:
        if not doc:
            doc = fget.__doc__
        super().__init__(fget, fset=fset, fdel=fdel, doc=doc)
        self.name: str = fget.__name__


class PersistentProperty(property):
    """A property that is persistent throughout device interaction.

    Value is queried only once and cached.
    """

    def __init__(self, fget: Callable[[Any], Any]) -> None:
        super().__init__(functools.lru_cache()(fget), doc=fget.__doc__)
        self.name: str = fget.__name__


class Affordance(property):
    """A property that represents an affordance."""

    def __init__(self, fget: Callable[[Any], Any]) -> None:
        super().__init__(functools.lru_cache()(fget), doc=fget.__doc__)
        self.name: str = fget.__name__


class Transport(property):
    """A property that represents an transport."""

    def __init__(self, fget: Callable[[Any], Any]) -> None:
        super().__init__(functools.lru_cache()(fget), doc=fget.__doc__)
        self.name: str = fget.__name__
