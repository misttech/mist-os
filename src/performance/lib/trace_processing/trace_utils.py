# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utilities to filter and extract events and statistics from a trace Model."""

from typing import Iterable, Optional, Type

import trace_processing.trace_model as trace_model


def filter_events(
    events: Iterable[trace_model.Event],
    category: Optional[str] = None,
    name: Optional[str] = None,
    type: Type = object,
) -> Iterable[trace_model.Event]:
    def event_matches(event: trace_model.Event) -> bool:
        type_matches = isinstance(event, type)
        category_matches: bool = category is None or event.category == category
        name_matches: bool = name is None or event.name == name
        return type_matches and category_matches and name_matches

    return (e for e in events if event_matches(e))
