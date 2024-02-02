# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utilities to filter and extract events and statistics from a trace Model."""

import math
from typing import Any, Iterable, List, Optional, Set, Tuple, Type

import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time


# Compute the mean (https://en.wikipedia.org/wiki/Arithmetic_mean#Definition)
# of [values].
def mean(values: Iterable[int | float]) -> float:
    if not values:
        raise TypeError("[values] must not be empty in order to compute mean")

    return sum(values) / len(values)


# Compute the population variance (https://en.wikipedia.org/wiki/Variance#Population_variance)
# of [values].
def variance(values: Iterable[int | float]) -> float:
    vales_mean: float = mean(values)
    return sum(((v - vales_mean) ** 2.0 for v in values)) / len(values)


# Compute the population standard deviation (https://en.wikipedia.org/wiki/Standard_deviation#Uncorrected_sample_standard_deviation)
# of [values].
def standard_deviation(values: Iterable[int | float]) -> float:
    return math.sqrt(variance(values))


# Compute the linear interpolated [percentile]th percentile
# (https://en.wikipedia.org/wiki/Percentile) of [values].
def percentile(values: Iterable[int | float], percentile: int) -> float:
    if not values:
        raise TypeError(
            "[values] must not be empty in order to compute percentile"
        )

    values_list: List[int | float] = sorted(values)
    if percentile == 100:
        return float(values_list[-1])

    index_as_float: float = float(len(values_list) - 1) * (0.01 * percentile)
    index: int = math.floor(index_as_float)
    if (index + 1) == len(values_list):
        return float(values_list[-1])
    index_fraction: float = index_as_float % 1
    return (
        values_list[index] * (1.0 - index_fraction)
        + values_list[index + 1] * index_fraction
    )


def filter_events(
    events: Iterable[trace_model.Event],
    category: Optional[str] = None,
    name: Optional[str] = None,
    type: Type = object,
) -> Iterable[trace_model.Event]:
    """Filter |events| based on category, name, or type.

    Args:
      events: The set of events to filter.
      category: Category of events to include, or None to skip this filter.
      name: name of events to include, or None to skip this filter.
      type: Type of events to include. By default object to include all events.

    Returns:
      An [Iterable] of filtered events.
    """

    def event_matches(event: trace_model.Event) -> bool:
        type_matches = isinstance(event, type)
        category_matches: bool = category is None or event.category == category
        name_matches: bool = name is None or event.name == name
        return type_matches and category_matches and name_matches

    return filter(event_matches, events)


def total_event_duration(
    events: Iterable[trace_model.Event],
) -> trace_time.TimeDelta:
    """Compute the total duration of all [Event]s in |events|.  This is the end
    of the last event minus the beginning of the first event.

    Args:
      events: The set of events to compute the duration for.

    Returns:
      Total event duration.
    """

    def event_times(
        e: trace_model.Event,
    ) -> Tuple[trace_time.TimePoint, trace_time.TimePoint]:
        end: trace_time.TimePoint = e.start
        if isinstance(e, (trace_model.AsyncEvent, trace_model.DurationEvent)):
            end = e.start + e.duration
        return (e.start, end)

    start_times, end_times = zip(*map(event_times, events))
    min_time: trace_time.TimePoint = min(
        start_times, default=trace_time.TimePoint.zero()
    )
    max_time: trace_time.TimePoint = max(
        end_times, default=trace_time.TimePoint.zero()
    )

    return max_time - min_time


def get_arg_values_from_events(
    events: Iterable[trace_model.Event],
    arg_key: str,
    arg_types: Type | Tuple[Type, ...] = object,
) -> Iterable[Any]:
    """Collect values from the |args| maps in |events|.

    Args:
      events: The events to collect args from.
      arg_key: The key in the |args| maps to collect values from.
      arg_type:

    Raises:
      KeyError: The value corresponding to |arg_key| was not present in one of
        the event's |args| map.

    Returns:
      An [Iterable] of collected values.
    """

    def event_to_arg_type(event: trace_model.Event) -> Any:
        has_arg: bool = arg_key in event.args
        arg: Any = event.args.get(arg_key, None)
        if not has_arg or not isinstance(arg, arg_types):
            raise KeyError(
                f"Error, expected events to include arg with key '{arg_key}' "
                f"of type(s) {str(arg_types)}"
            )
        return arg  # Cannot be None if we reach here

    return map(event_to_arg_type, events)


def get_following_events(
    event: trace_model.Event,
) -> Iterable[trace_model.Event]:
    """Find all Events that are flow connected and follow |event|.

    Args:
      event: The starting event.

    Returns:
      An [Iterable] of flow connected events.
    """
    frontier: List[trace_model.Event] = [event]
    visited: Set[trace_model.Event] = set()

    def set_add(
        event_set: Set[trace_model.Event], event: trace_model.Event
    ) -> bool:
        length_before = len(event_set)
        event_set.add(event)
        return len(event_set) != length_before

    while frontier:
        current: trace_model.Event = frontier.pop()
        if current is None:
            continue
        added = set_add(visited, current)
        if not added:
            continue
        if isinstance(current, trace_model.DurationEvent):
            frontier.extend(current.child_durations)
            frontier.extend(current.child_flows)
        elif isinstance(current, trace_model.FlowEvent):
            frontier.append(current.enclosing_duration)
            frontier.append(current.next_flow)

    def by_start_time(event: trace_model.Event) -> trace_time.TimePoint:
        return event.start

    for connected_event in sorted(visited, key=by_start_time):
        yield connected_event


def get_nearest_following_event(
    event: trace_model.Event,
    following_event_category: str,
    following_event_name: str,
) -> trace_model.Event:
    """Find the nearest target event that is flow connected and follow |event|.

    Args:
      event: The starting event.
      following_event_category: Trace category of the target event.
      following_event_name: Trace event name of the target event.

    Returns:
      Flow connected events. If nothing is found, None.
    """
    filtered_following_events = filter_events(
        get_following_events(event),
        category=following_event_category,
        name=following_event_name,
        type=trace_model.DurationEvent,
    )
    return next(filtered_following_events, None)
