#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import itertools
import logging
import statistics
import sys
from typing import Any, Iterable, Iterator, Self, Sequence, TypeAlias

from trace_processing import trace_metrics, trace_model, trace_time, trace_utils

_LOGGER: logging.Logger = logging.getLogger(__name__)
_CPU_USAGE_EVENT_NAME = "cpu_usage"
_DEFAULT_PERCENT_CUTOFF = 0.0

Breakdown: TypeAlias = list[dict[str, trace_metrics.JSON]]


class CpuMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes CPU utilization metrics, both structured and freeform.

    CPU load metrics are reported as a percentage of the total load across all cores, with 100%
    mapping to full utilization of every core simultaneously. Some tools will report a process using
    both cores on a dual core system as taking up 200% of CPU; that is not the case here. Depending
    on configuration, this module will report a time series of average utiliztion samples taken
    periodically for the duration of the test, or aggregate measurements-- such as min, max,
    average, and percentiles -- calculated over that data.

    Freeform CPU load metrics report per-core average load for every thread in every process on the
    system. These data are not suited for automated changepoint detection, and so are instead
    piped to a dashboard.
    """

    FREEFORM_METRICS_FILENAME = "cpu_breakdown"

    def __init__(
        self,
        aggregates_only: bool = True,
        percent_cutoff: float = _DEFAULT_PERCENT_CUTOFF,
    ):
        """Constructor.

        Args:
            aggregates_only: When True, generates CpuMin, CpuMax, CpuAverage and CpuP* (%iles).
                Otherwise generates CpuLoad metric with all cpu values.
            percent_cutoff: Any process that has CPU below this won't be listed in the CPU usage
                breakdown reported in freeform metrics.
        """
        self.aggregates_only: bool = aggregates_only
        self._percent_cutoff = percent_cutoff

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[trace_metrics.TestCaseResult]:
        all_events: Iterator[trace_model.Event] = model.all_events()
        cpu_usage_events: Iterable[
            trace_model.Event
        ] = trace_utils.filter_events(
            all_events,
            category="system_metrics_logger",
            name=_CPU_USAGE_EVENT_NAME,
            type=trace_model.CounterEvent,
        )
        cpu_percentages: list[float] = list(
            trace_utils.get_arg_values_from_events(
                cpu_usage_events,
                arg_key=_CPU_USAGE_EVENT_NAME,
                arg_types=(int, float),
            )
        )

        # TODO(b/156300857): Remove this fallback after all consumers have been
        # updated to use system_metrics_logger.
        if len(cpu_percentages) == 0:
            all_events = model.all_events()
            cpu_usage_events = trace_utils.filter_events(
                all_events,
                category="system_metrics",
                name=_CPU_USAGE_EVENT_NAME,
                type=trace_model.CounterEvent,
            )
            cpu_percentages = list(
                trace_utils.get_arg_values_from_events(
                    cpu_usage_events,
                    arg_key="average_cpu_percentage",
                    arg_types=(int, float),
                )
            )

        if len(cpu_percentages) == 0:
            _LOGGER.info(
                "No cpu usage measurements are present. Perhaps the trace "
                "duration is too short to provide cpu usage information"
            )
            return []

        cpu_mean: float = statistics.mean(cpu_percentages)
        _LOGGER.info(f"Average CPU Load: {cpu_mean}")

        if self.aggregates_only:
            return trace_utils.standard_metrics_set(
                values=cpu_percentages,
                label_prefix="Cpu",
                unit=trace_metrics.Unit.percent,
            )
        return [
            trace_metrics.TestCaseResult(
                "CpuLoad", trace_metrics.Unit.percent, cpu_percentages
            )
        ]

    def process_freeform_metrics(
        self, model: trace_model.Model
    ) -> tuple[str, Breakdown]:
        """
        Given trace_model.Model, iterates through all the SchedulingRecords and calculates the
        duration for each Process's Threads, and saves them by CPU.

        Args:
            model: The input trace model.

        Returns:
            str: stable identifier to use in freeform metrics file name.
            Breakdown: Per-process, per-thread CPU usage breakdown.
        """
        (breakdown, _) = self.process_metrics_and_get_total_time(model)
        return self.FREEFORM_METRICS_FILENAME, breakdown

    def process_metrics_and_get_total_time(
        self, model: trace_model.Model
    ) -> tuple[Breakdown, float]:
        """
        Given trace_model.Model, iterates through all the SchedulingRecords and calculates the
        duration for each Process's Threads, and saves them by CPU.

        Args:
            model: The input trace model.

        Returns:
            Breakdown: Per-process, per-thread CPU usage breakdown.
            float: The total duration of the trace.
        """
        # Map tids to names.
        tid_to_process_name: dict[int, str] = {}
        tid_to_thread_name: dict[int, str] = {}
        for p in model.processes:
            for t in p.threads:
                tid_to_process_name[t.tid] = p.name
                tid_to_thread_name[t.tid] = t.name

        # Calculate durations for each CPU for each tid.
        durations = DurationsBreakdown.calculate(
            model.scheduling_records,
            tid_to_thread_name,
        )

        # Calculate the percent of time the thread spent on this CPU,
        # compared to the total CPU duration.
        # If the percent spent is at or above our cutoff, add metric to
        # breakdown.
        full_breakdown: list[dict[str, trace_metrics.JSON]] = []
        for tid, breakdown in durations.tid_to_durations.items():
            if tid in tid_to_thread_name:
                for cpu, duration in breakdown.items():
                    percent = (
                        duration / durations.cpu_to_total_duration[cpu] * 100
                    )
                    if percent >= self._percent_cutoff:
                        metric: dict[str, trace_metrics.JSON] = {
                            "process_name": tid_to_process_name[tid],
                            "thread_name": tid_to_thread_name[tid],
                            "tid": tid,
                            "cpu": cpu,
                            "percent": percent,
                            "duration": duration,
                        }
                        full_breakdown.append(metric)

        if durations.cpu_to_skipped_duration:
            _LOGGER.warning(
                "Possibly missing ContextSwitch record(s) in trace for these CPUs and durations: "
                f"{durations.cpu_to_skipped_duration}"
            )

        # Sort metrics by CPU (desc) and percent (desc).
        full_breakdown.sort(
            key=lambda m: (m["cpu"], m["percent"]),
            reverse=True,
        )
        return full_breakdown, durations.max_timestamp - durations.min_timestamp


class DurationsBreakdown:
    def __init__(self) -> None:
        # Maps TID to a dict of CPUs to total duration (ms) on that CPU.
        # E.g. For a TID of 1001 with 3 CPUs, this would be:
        #   {1001: {0: 1123.123, 1: 123123.123, 3: 1231.23}}
        self.tid_to_durations: dict[int, dict[int, float]] = {}
        # Map of CPU to total duration used (ms).
        self.cpu_to_total_duration: dict[int, float] = {}
        self.cpu_to_skipped_duration: dict[int, float] = {}
        self.min_timestamp: float = sys.float_info.max
        self.max_timestamp: float = 0

    def _calculate_duration_per_cpu(
        self,
        cpu: int,
        records: list[trace_model.ContextSwitch],
        tid_to_thread_name: dict[int, str],
    ) -> None:
        """
        Calculates the total duration for each thread, on a particular CPU.

        Uses a list of sorted ContextSwitch records to sum up the duration for each thread.
        It's possible that consecutive records do not have matching incoming_tid and outgoing_tid.
        """
        smallest_timestamp = self._timestamp_ms(records[0].start)
        if smallest_timestamp < self.min_timestamp:
            self.min_timestamp = smallest_timestamp
        largest_timestamp = self._timestamp_ms(records[-1].start)
        if largest_timestamp > self.max_timestamp:
            self.max_timestamp = largest_timestamp
        total_duration = largest_timestamp - smallest_timestamp
        skipped_duration = 0.0
        self.cpu_to_total_duration[cpu] = total_duration

        for prev_record, curr_record in itertools.pairwise(records):
            # Check that the previous ContextSwitch's incoming_tid ("this thread is starting work
            # on this CPU") matches the current ContextSwitch's outgoing_tid ("this thread is being
            # switched away from"). If so, there is a duration to calculate. Otherwise, it means
            # maybe there is skipped data or something.
            if prev_record.tid != curr_record.outgoing_tid:
                start_ts = self._timestamp_ms(prev_record.start)
                stop_ts = self._timestamp_ms(curr_record.start)
                skipped_duration += stop_ts - start_ts
            # Purposely skip saving idle thread durations.
            elif prev_record.is_idle():
                continue
            else:
                start_ts = self._timestamp_ms(prev_record.start)
                stop_ts = self._timestamp_ms(curr_record.start)
                duration = stop_ts - start_ts
                assert duration >= 0
                if curr_record.outgoing_tid in tid_to_thread_name:
                    # Add duration to the total duration for that tid and CPU.
                    self.tid_to_durations.setdefault(
                        curr_record.outgoing_tid, {}
                    ).setdefault(cpu, 0)
                    self.tid_to_durations[curr_record.outgoing_tid][
                        cpu
                    ] += duration

        if skipped_duration > 0:
            self.cpu_to_skipped_duration[cpu] = skipped_duration

    @staticmethod
    def _timestamp_ms(timestamp: trace_time.TimePoint) -> float:
        """
        Return timestamp in ms.
        """
        return timestamp.to_epoch_delta().to_milliseconds_f()

    @classmethod
    def calculate(
        cls,
        per_cpu_scheduling_records: dict[
            int, list[trace_model.SchedulingRecord]
        ],
        tid_to_thread_name: dict[int, str],
    ) -> Self:
        durations = cls()
        for cpu, records in per_cpu_scheduling_records.items():
            durations._calculate_duration_per_cpu(
                cpu,
                sorted(
                    trace_utils.filter_records(
                        records, trace_model.ContextSwitch
                    ),
                    key=lambda record: record.start,
                ),
                tid_to_thread_name,
            )
        return durations


def group_by_process_name(breakdown: Breakdown) -> Breakdown:
    """
    Given a breakdown, group the metrics by process_name only,
    ignoring thread name.
    """
    breakdown.sort(key=lambda m: (m["cpu"], m["process_name"]))
    if not breakdown:
        return []
    consolidated_breakdown: Breakdown = [breakdown[0]]
    for metric in breakdown[1:]:
        if (
            metric["cpu"] == consolidated_breakdown[-1]["cpu"]
            and metric["process_name"]
            == consolidated_breakdown[-1]["process_name"]
        ):
            consolidated_breakdown[-1] = _merge(
                metric, consolidated_breakdown[-1]
            )
        else:
            metric.pop("thread_name", None)
            metric.pop("tid", None)
            consolidated_breakdown.append(metric)

    return sorted(
        consolidated_breakdown,
        key=lambda m: (m["cpu"], m["percent"]),
        reverse=True,
    )


def _merge(a: dict[str, Any], b: dict[str, Any]) -> dict[str, Any]:
    return {
        "process_name": a["process_name"],
        "cpu": a["cpu"],
        "duration": a["duration"] + b["duration"],
        "percent": a["percent"] + b["percent"],
    }
