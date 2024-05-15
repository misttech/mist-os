# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utilities to filter and extract events and statistics from a trace Model."""

import math
import statistics
from typing import Any, Iterable, Iterator, List, Optional, Set, Tuple, TypeVar

from trace_processing import trace_model, trace_metrics, trace_time


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
    type: type = object,
) -> Iterator[trace_model.Event]:
    """Filter |events| based on category, name, or type.

    Args:
      events: The set of events to filter.
      category: Category of events to include, or None to skip this filter.
      name: name of events to include, or None to skip this filter.
      type: Type of events to include. By default object to include all events.

    Returns:
      An [Iterator] of filtered events. Note that Iterators can only be iterated a single time, so
      the caller must create a local copy in order to loop over the filtered events more than once.
    """

    def event_matches(event: trace_model.Event) -> bool:
        type_matches = isinstance(event, type)
        category_matches: bool = category is None or event.category == category
        name_matches: bool = name is None or event.name == name
        return type_matches and category_matches and name_matches

    return filter(event_matches, events)


U = TypeVar("U", bound=trace_model.SchedulingRecord)


def filter_records(
    records: Iterable[trace_model.SchedulingRecord], type_: type[U]
) -> Iterator[U]:
    """Filters SchedulingRecords by type.

    Easier for mypy to grok than using types with filter().

    Args:
        records: Iterable of SchedulingRecords to be filtered.
        type_: The subclass of SchedulingRecord by which to filter.

    Yields:
        The elements of the iterable that are of the specified type.
    """
    for record in records:
        if isinstance(record, type_):
            yield record


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
        if (
            isinstance(e, trace_model.AsyncEvent | trace_model.DurationEvent)
            and e.duration
        ):
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
    arg_types: type | Tuple[type, ...] = object,
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
        added = set_add(visited, current)
        if not added:
            continue
        if isinstance(current, trace_model.DurationEvent):
            frontier.extend(current.child_durations)
            frontier.extend(current.child_flows)
        elif isinstance(current, trace_model.FlowEvent):
            if current.enclosing_duration:
                frontier.append(current.enclosing_duration)
            if current.next_flow:
                frontier.append(current.next_flow)

    def by_start_time(event: trace_model.Event) -> trace_time.TimePoint:
        return event.start

    for connected_event in sorted(visited, key=by_start_time):
        yield connected_event


def get_nearest_following_event(
    event: trace_model.Event,
    following_event_category: str,
    following_event_name: str,
) -> trace_model.Event | None:
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
    return next(iter(filtered_following_events), None)


# This method looks for a possible race between trace collection start in multiple processes.
#
# This problem usually occurs between Scenic and Magma processes. The flow connection between
# these processes happen through koids of objects passed to Vulkan, which may be reused. When
# there is a gap in process trace collection starts, we may see the initial vsync event(s)
# connecting to multiple flows. We can safely skip these initial events. See
# https://fxbug.dev/322849857 for more detailed examples with screenshots.
def adjust_to_common_process_start(
    model: trace_model.Model, process_and_thread_names: List[tuple[str, str]]
) -> trace_model.Model:
    """Adjust model to a consistent start time tracking the latest first event recorded from a
    list of processes. The list of processes are selected through matching process_name exactly
    and thread_name as substring.

    Args:
      model: Trace model.
      process_and_thread_names: Tuples of strings representing (process_name, thread_name).

    Returns:
      Model adjusted to a start time tracking the latest first event recorded from the given
      processes. KeyError if no match is found from the process and thread name keys.
    """
    processes = []
    for process_name, thread_name in process_and_thread_names:
        process_matches = [p for p in model.processes if process_name == p.name]
        if not process_matches:
            raise KeyError(
                f"Error, expected traces with process_name '{process_name}'"
            )
        for i, process_match in enumerate(process_matches):
            thread_match = next(
                (t for t in process_match.threads if thread_name in t.name),
                None,
            )
            if thread_match:
                processes.append(process_match)
                break
            elif i == len(process_matches) - 1:
                raise KeyError(
                    f"Error, expected traces with process_name '{process_name}' "
                    f"and thread name `{thread_name}`"
                )
    consistent_start_time = max(
        min(
            (
                event.start
                for thread in process.threads
                for event in thread.events
            ),
            default=trace_time.TimePoint(),
        )
        for process in processes
    )
    return model.slice(start=consistent_start_time)


def standard_metrics_set(
    values: List[float],
    label_prefix: str,
    unit: trace_metrics.Unit,
    percentiles: tuple[int, int, int, int, int] = (5, 25, 50, 75, 95),
) -> list[trace_metrics.TestCaseResult]:
    """Generates min, max, average and percentiles metrics for the given values.

    Args:
        values: Input to create the metrics from.
        label_prefix: metric labels will be '{label_prefix}Min', '{label_prefix}Max',
            '{label_prefix}Average' and '{label_prefix}P*' (percentiles).
        unit: The metrics unit.
        percentiles: Percentiles to output.

    Returns:
        A list of TestCaseResults representing each of the generated metrics.
    """

    results = [
        trace_metrics.TestCaseResult(
            f"{label_prefix}P{p}",
            unit,
            [percentile(values, p)],
        )
        for p in percentiles
    ]

    results += [
        trace_metrics.TestCaseResult(f"{label_prefix}Min", unit, [min(values)]),
        trace_metrics.TestCaseResult(f"{label_prefix}Max", unit, [max(values)]),
        trace_metrics.TestCaseResult(
            f"{label_prefix}Average", unit, [statistics.mean(values)]
        ),
    ]

    return results
