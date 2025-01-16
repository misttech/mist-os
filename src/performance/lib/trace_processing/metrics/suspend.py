# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Suspend trace metrics."""

import itertools
import logging
from typing import Iterator, Optional, Sequence, Tuple

from trace_processing import trace_metrics, trace_model, trace_time, trace_utils

_LOGGER: logging.Logger = logging.getLogger(__name__)

_EVENT_CATEGORY = "power"

# LINT.IfChange
_SYSFS_EVENT_NAME = "starnix-sysfs:suspend"
# LINT.ThenChange(//src/starnix/kernel/power/state.rs)

# LINT.IfChange
_STARNIX_RUNNER_SUSPEND_EVENT_NAME = (
    "starnix-runner:drop-application-activity-lease"
)
_STARNIX_RUNNER_RESUME_EVENT_NAME = (
    "starnix-runner:acquire-application-activity-lease"
)
# LINT.ThenChange(//src/starnix/lib/kernel_manager/src/kernels.rs)

# LINT.IfChange
_SAG_EVENT_NAME = "system-activity-governor:suspend"
# LINT.ThenChange(//src/power/system-activity-governor/src/cpu_manager.rs)

# LINT.IfChange
_SUSPEND_EVENT_NAME = "generic-suspend:suspend"
# LINT.ThenChange(//src/devices/suspend/drivers/generic-suspend/generic-suspend.cc)


class SuspendMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes suspend/resume metrics."""

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[trace_metrics.TestCaseResult]:
        """Calculate suspend/resume metrics.

        Args:
            model: In-memory representation of a system trace.

        Returns:
            Set of metrics results for this test case.
        """

        def unwrap(e: Optional[trace_time.TimePoint]) -> trace_time.TimePoint:
            if e is None:
                raise ValueError("expected some, but got None")
            return e

        # Suspend-resume multi-step measurements. This section calculates the
        # time taken for each step in the suspend/resume process, averaged
        # across multiple occurrences.
        #
        # The steps measured are:
        # 1. sysfs suspend start -> Starnix kernel suspend start
        # 2. sysfs suspend start -> SAG suspend start
        # 3. sysfs suspend start -> AML suspend driver start
        # 4. sysfs suspend start -> CPU idle start
        # 5. CPU idle end -> AML suspend driver end
        # 6. CPU idle end -> SAG suspend end
        # 7. CPU idle end -> Starnix kernel resume end
        # 8. CPU idle end -> sysfs suspend end
        sysfs_events = filter_sysfs_suspend_events(model)
        suspend_cnt = 0
        suspend_resume_steps_sum = [trace_time.TimeDelta(0)] * 8
        for sysfs_event in sysfs_events:
            if sysfs_event.duration is not None:
                suspend_resume_steps = get_time_steps(model, sysfs_event)
                if suspend_resume_steps is not None:
                    suspend_resume_steps_sum = [
                        s + t
                        for s, t in zip(
                            suspend_resume_steps_sum, suspend_resume_steps
                        )
                    ]
                    suspend_cnt += 1

        sag_events = filter_sag_suspend_events(model)
        suspend_time = trace_time.TimeDelta(0)
        for sag_event in sag_events:
            if sag_event.duration is not None:
                suspend_time += sag_event.duration

        # TODO(https://fxbug.dev/366507238): Find a more robust way of
        # determining the start of the test. Doing so would be valuable
        # for tests that do not include scheduling events.
        trace_start_time = min(
            map(
                lambda e: e.start,
                sum(model.scheduling_records.values(), []),
            )
        )
        event_end_times = map(lambda e: e.end_time(), model.all_events())
        trace_end_time = max(
            [unwrap(e) for e in event_end_times if e is not None]
        )
        total_time = trace_end_time - trace_start_time
        running_time = total_time - suspend_time

        result = [
            trace_metrics.TestCaseResult(
                label="UnsuspendedTime",
                unit=trace_metrics.Unit.nanoseconds,
                values=[running_time.to_nanoseconds()],
            ),
            trace_metrics.TestCaseResult(
                label="SuspendTime",
                unit=trace_metrics.Unit.nanoseconds,
                values=[suspend_time.to_nanoseconds()],
            ),
            trace_metrics.TestCaseResult(
                label="SuspendPercentage",
                unit=trace_metrics.Unit.percent,
                values=[(suspend_time / total_time) * 100],
            ),
        ]

        if suspend_cnt > 0:
            result.extend(
                [
                    trace_metrics.TestCaseResult(
                        label="Suspend.sysfs_to_starnix_kernel",
                        unit=trace_metrics.Unit.nanoseconds,
                        values=[
                            suspend_resume_steps_sum[0].to_nanoseconds()
                            / suspend_cnt
                        ],
                    ),
                    trace_metrics.TestCaseResult(
                        label="Suspend.sysfs_to_sag",
                        unit=trace_metrics.Unit.nanoseconds,
                        values=[
                            suspend_resume_steps_sum[1].to_nanoseconds()
                            / suspend_cnt
                        ],
                    ),
                    trace_metrics.TestCaseResult(
                        label="Suspend.sysfs_to_suspend_driver",
                        unit=trace_metrics.Unit.nanoseconds,
                        values=[
                            suspend_resume_steps_sum[2].to_nanoseconds()
                            / suspend_cnt
                        ],
                    ),
                    trace_metrics.TestCaseResult(
                        label="Suspend.sysfs_to_cpu_idle",
                        unit=trace_metrics.Unit.nanoseconds,
                        values=[
                            suspend_resume_steps_sum[3].to_nanoseconds()
                            / suspend_cnt
                        ],
                    ),
                    trace_metrics.TestCaseResult(
                        label="Resume.cpu_idle_to_suspend_driver",
                        unit=trace_metrics.Unit.nanoseconds,
                        values=[
                            suspend_resume_steps_sum[4].to_nanoseconds()
                            / suspend_cnt
                        ],
                    ),
                    trace_metrics.TestCaseResult(
                        label="Resume.cpu_idle_to_sag",
                        unit=trace_metrics.Unit.nanoseconds,
                        values=[
                            suspend_resume_steps_sum[5].to_nanoseconds()
                            / suspend_cnt
                        ],
                    ),
                    trace_metrics.TestCaseResult(
                        label="Resume.cpu_idle_to_starnix_kernel",
                        unit=trace_metrics.Unit.nanoseconds,
                        values=[
                            suspend_resume_steps_sum[6].to_nanoseconds()
                            / suspend_cnt
                        ],
                    ),
                    trace_metrics.TestCaseResult(
                        label="Resume.cpu_idle_to_sysfs",
                        unit=trace_metrics.Unit.nanoseconds,
                        values=[
                            suspend_resume_steps_sum[7].to_nanoseconds()
                            / suspend_cnt
                        ],
                    ),
                ]
            )
        return result


def filter_sysfs_suspend_events(
    model: trace_model.Model,
) -> Iterator[trace_model.DurationEvent]:
    """Extract sysfs suspend duration events from the provided trace model.

    Args:
        model: In-memory representation of a system trace.

    Returns:
        Iterator of sysfs suspend duration events emitted by the power subsystem.

    """
    return trace_utils.filter_events(
        model.all_events(),
        category=_EVENT_CATEGORY,
        name=_SYSFS_EVENT_NAME,
        type=trace_model.DurationEvent,
    )


def filter_sag_suspend_events(
    model: trace_model.Model,
) -> Iterator[trace_model.DurationEvent]:
    """Extract SAG suspend duration events from the provided trace model.

    Args:
        model: In-memory representation of a system trace.

    Returns:
        Iterator of SAG suspend duration events emitted by the power subsystem.

    """
    return trace_utils.filter_events(
        model.all_events(),
        category=_EVENT_CATEGORY,
        name=_SAG_EVENT_NAME,
        type=trace_model.DurationEvent,
    )


def get_time_steps(
    model: trace_model.Model,
    sysfs_event: trace_model.DurationEvent,
) -> list[trace_time.TimeDelta] | None:
    """Calculates the time steps between key events during a suspend/resume operation.

    This function analyzes a trace model to identify specific events related to
    suspend/resume functionality and calculates the time deltas between them.
    It focuses on events from Starnix kernel, SAG, AML driver, and CPU idle states.

    Args:
        model: The trace model containing the events.
        sysfs_event: The DurationEvent representing the overall suspend/resume
                     operation from the sysfs perspective.

    Returns:
        A list of TimeDelta objects representing the time steps between the
        following events:
            - sysfs suspend start -> Starnix kernel suspend start
            - sysfs suspend start -> SAG suspend start
            - sysfs suspend start -> AML driver suspend start
            - sysfs suspend start -> CPU idle start
            - CPU idle end -> AML driver suspend end
            - CPU idle end -> SAG suspend end
            - CPU idle end -> Starnix kernel resume end
            - CPU idle end -> sysfs suspend end

        Returns None if any of the required events are not found or if their
        timing information is incomplete.
    """

    def unwrap(e: Optional[trace_time.TimePoint]) -> trace_time.TimePoint:
        """
        Helper function to unwrap an optional TimePoint.

        Raises a ValueError if the TimePoint is None.
        """
        if e is None:
            raise ValueError("expected some, but got None")
        return e

    # Slice the model to focus on the suspend/resume interval
    suspend_resume_model = model.slice(
        sysfs_event.start, sysfs_event.end_time()
    )

    # Extract the relevant events
    starnix_kernel_suspend_event = get_starnix_kernel_suspend_event(
        suspend_resume_model
    )
    if starnix_kernel_suspend_event is not None:
        starnix_kernel_suspend_start = starnix_kernel_suspend_event.start
    else:
        _LOGGER.warning("Starnix kernel suspend event not found.")
        return None

    sag_event = get_sag_event(suspend_resume_model)
    if sag_event is not None and sag_event.duration is not None:
        sag_suspend_start = sag_event.start
        sag_suspend_end = unwrap(sag_event.end_time())
    else:
        _LOGGER.warning("SAG event not found or incomplete.")
        return None

    aml_driver_event = get_aml_driver_event(suspend_resume_model)
    if aml_driver_event is not None and aml_driver_event.duration is not None:
        aml_suspend_start = aml_driver_event.start
        aml_suspend_end = unwrap(aml_driver_event.end_time())
    else:
        _LOGGER.warning("AML driver event not found or incomplete.")
        return None

    starnix_kernel_resume_event = get_starnix_kernel_resume_event(
        suspend_resume_model
    )
    if (
        starnix_kernel_resume_event is not None
        and starnix_kernel_resume_event.duration is not None
    ):
        starnix_kernel_resume_end = unwrap(
            starnix_kernel_resume_event.end_time()
        )
    else:
        _LOGGER.warning("Starnix kernel resume event not found or incomplete.")
        return None

    # Get CPU idle time within the AML driver suspend interval
    cpu_idle_time = get_cpu_idle_time(
        model,
        aml_suspend_start,
        aml_suspend_end,
    )
    if cpu_idle_time is not None and len(cpu_idle_time) == 2:
        cpu_idle_start = cpu_idle_time[0]
        cpu_idle_end = cpu_idle_time[1]
    else:
        _LOGGER.warning("CPU idle time not found.")
        return None

    return [
        starnix_kernel_suspend_start - sysfs_event.start,
        sag_suspend_start - sysfs_event.start,
        aml_suspend_start - sysfs_event.start,
        cpu_idle_start - sysfs_event.start,
        aml_suspend_end - cpu_idle_end,
        sag_suspend_end - cpu_idle_end,
        starnix_kernel_resume_end - cpu_idle_end,
        unwrap(sysfs_event.end_time()) - cpu_idle_end,
    ]


def get_sag_event(
    model: trace_model.Model,
) -> trace_model.DurationEvent | None:
    """Retrieves the SAG suspend event from the trace model.

    It is designed for use cases where only a single suspend/resume
    operation is expected within the trace model.

    Args:
        model: In-memory representation of a system trace.

    Returns:
        The SAG suspend event if found, otherwise None.
        Raises a ValueError if more than one event is found.
    """
    sag_events = list(
        trace_utils.filter_events(
            model.all_events(),
            category=_EVENT_CATEGORY,
            name=_SAG_EVENT_NAME,
            type=trace_model.DurationEvent,
        )
    )
    if len(sag_events) == 1:
        return sag_events[0]
    elif len(sag_events) > 1:
        raise ValueError("Got more than one SAG suspend events")
    return None


def get_aml_driver_event(
    model: trace_model.Model,
) -> trace_model.DurationEvent | None:
    """Retrieves the aml driver suspend event from the trace model.

    It is designed for use cases where only a single suspend/resume
    operation is expected within the trace model.

    Args:
        model: In-memory representation of a system trace.

    Returns:
        The aml driver suspend event if found, otherwise None.
        Raises a ValueError if more than one event is found.
    """
    aml_driver_event = list(
        trace_utils.filter_events(
            model.all_events(),
            category=_EVENT_CATEGORY,
            name=_SUSPEND_EVENT_NAME,
            type=trace_model.DurationEvent,
        )
    )
    if len(aml_driver_event) == 1:
        return aml_driver_event[0]
    elif len(aml_driver_event) > 1:
        raise ValueError("Got more than one AML driver suspend events")
    return None


def get_starnix_kernel_suspend_event(
    model: trace_model.Model,
) -> trace_model.InstantEvent | None:
    """Retrieves the starnix kernel suspend event from the trace model.

    It is designed for use cases where only a single suspend/resume
    operation is expected within the trace model.

    Args:
        model: In-memory representation of a system trace.

    Returns:
        The starnix kernel suspend event if found, otherwise None.
        Raises a ValueError if more than one event is found.
    """
    starnix_kernel_suspend_event = list(
        trace_utils.filter_events(
            model.all_events(),
            category=_EVENT_CATEGORY,
            name=_STARNIX_RUNNER_SUSPEND_EVENT_NAME,
            type=trace_model.InstantEvent,
        )
    )
    if len(starnix_kernel_suspend_event) == 1:
        return starnix_kernel_suspend_event[0]
    elif len(starnix_kernel_suspend_event) > 1:
        raise ValueError("Got more than one starnix kernel suspend events")
    return None


def get_starnix_kernel_resume_event(
    model: trace_model.Model,
) -> trace_model.DurationEvent | None:
    """Retrieves the starnix kernel resume event from the trace model.

    It is designed for use cases where only a single suspend/resume
    operation is expected within the trace model.

    Args:
        model: In-memory representation of a system trace.

    Returns:
        The starnix kernel resume event if found, otherwise None.
        Raises a ValueError if more than one event is found.
    """
    starnix_kernel_resume_event = list(
        trace_utils.filter_events(
            model.all_events(),
            category=_EVENT_CATEGORY,
            name=_STARNIX_RUNNER_RESUME_EVENT_NAME,
            type=trace_model.DurationEvent,
        )
    )
    if len(starnix_kernel_resume_event) == 1:
        return starnix_kernel_resume_event[0]
    elif len(starnix_kernel_resume_event) > 1:
        raise ValueError("Got more than one starnix kernel resume events")
    return None


def get_cpu_idle_time(
    model: trace_model.Model,
    start: trace_time.TimePoint,
    end: trace_time.TimePoint,
) -> Tuple[trace_time.TimePoint, trace_time.TimePoint] | None:
    """Calculates the longest common CPU idle time across all CPUs in the model
    within a specified time window.

    This function identifies idle time intervals for each CPU overlap with or
    within the given [start, end] time range and then determines the longest
    continuous period where all CPUs were idle.

    Args:
        model: The trace model containing CPU scheduling records.
        start: The start time of the analysis window.
        end: The end time of the analysis window.

    Returns:
        A tuple of TimePoints representing the start and end of the longest
        common CPU idle interval within the [start, end] window.
        Returns None if no common idle time is found.
    """
    idle_times_list = []
    # Iterate over scheduling records for each CPU
    for cpu, records in model.scheduling_records.items():
        context_switch_records = [
            record
            for record in records
            if isinstance(record, trace_model.ContextSwitch)
        ]
        idle_times_list.append(
            get_per_cpu_idle_times(
                # All the records are sorted by start time
                sorted(
                    context_switch_records,
                    key=lambda record: record.start,
                ),
                start,
                end,
            )
        )
    common_idle_times = find_common_idle_times(idle_times_list, start, end)
    if not common_idle_times:
        return None
    return max(
        common_idle_times, key=lambda interval: interval[1] - interval[0]
    )


def get_per_cpu_idle_times(
    records: list[trace_model.SchedulingRecord],
    start: trace_time.TimePoint,
    end: trace_time.TimePoint,
) -> list[Tuple[trace_time.TimePoint, trace_time.TimePoint]]:
    """Calculates the idle time intervals for a single CPU within a specified
    time window, based on scheduling records.

    This function iterates through the scheduling records and identifies periods
    where the CPU is in an idle state, considering only intervals that overlap with
    or fall within the [start, end] time range. It assumes the records are sorted by
    start time.

    Args:
        records: A list of SchedulingRecord objects for a specific CPU, sorted
                 by start time.
        start: The start time of the analysis window.
        end: The end time of the analysis window.

    Returns:
        A list of tuples representing the idle time intervals within the
        [start, end] window. Each tuple contains the start and end TimePoints
        of an idle interval.
    """
    per_cpu_idle_times = []
    for prev_record, curr_record in itertools.pairwise(records):
        # Find intervals that overlap with or fall within the [start, end] window.
        # A CPU might have idle threads scheduled before AML driver suspension
        if (
            prev_record.is_idle()
            and start < curr_record.start
            and prev_record.start < end
        ):
            per_cpu_idle_times.append((prev_record.start, curr_record.start))
    return per_cpu_idle_times


def find_common_idle_times(
    idle_times_list: list[
        list[Tuple[trace_time.TimePoint, trace_time.TimePoint]]
    ],
    boundry_start: trace_time.TimePoint,
    boundry_end: trace_time.TimePoint,
) -> list[Tuple[trace_time.TimePoint, trace_time.TimePoint]]:
    """Finds the common idle time intervals across multiple lists of
    idle time intervals, within a specified time window.

    Args:
      idle_times_list: A list of lists, where each inner list contains tuples
                       representing idle time intervals (start_timestamp,
                       end_timestamp).
      boundry_start: The start time of the analysis window.
      boundry_end: The end time of the analysis window.

    Returns:
      A list of tuples representing the common idle time intervals.
    """

    if not idle_times_list:
        return []

    # Start with the first list as the initial common intervals
    common_intervals = idle_times_list[0]

    # Add the boundary window as a "virtual" list to constrain the results
    # within the [boundry_start, boundry_end] time range. This is needed
    # because idle_times_list might contain time intervals that overlap
    # with the analysis window, not just intervals that fall entirely
    # within it.
    idle_times_list.append([(boundry_start, boundry_end)])

    for idle_times in idle_times_list[1:]:
        new_common_intervals = []
        # Since each list only contains a few idle intervals from a single
        # driver suspension period, performance isn't a concern.
        for interval1 in common_intervals:
            for interval2 in idle_times:
                start = max(interval1[0], interval2[0])
                end = min(interval1[1], interval2[1])
                if start < end:
                    new_common_intervals.append((start, end))
        common_intervals = new_common_intervals

    return common_intervals


def make_synthetic_event(
    timestamp_usec: int, pid: int, tid: int, duration_usec: int
) -> trace_model.DurationEvent:
    """Build a synthetic suspend DurationEvent.

    Providing this function enables building fake traces for unittests while
    preventing the event category and name from leaking outside this module.
    """
    return trace_model.DurationEvent.from_dict(
        {
            "cat": _EVENT_CATEGORY,
            "name": _SAG_EVENT_NAME,
            "ts": timestamp_usec,
            "pid": pid,
            "tid": tid,
            "dur": duration_usec,
        }
    )
