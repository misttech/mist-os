#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from enum import Enum
from typing import Dict, List, Tuple
import itertools
import logging
import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time

_LOGGER: logging.Logger = logging.getLogger("CpuBreakdownMetricsProcessor")

# Default cut-off for the percentage CPU. Any process that has CPU below this
# won't be listed in the results. User can pass in a cutoff.
DEFAULT_PERCENT_CUTOFF = 0.0


class CpuBreakdownMetricsProcessor:
    """
    Breaks down CPU metrics into a free-form metrics format.
    """

    def __init__(
        self,
        model: trace_model.Model,
        output_path: str = "",
        percent_cutoff: float = DEFAULT_PERCENT_CUTOFF,
    ) -> None:
        self._model: trace_model.Model = model
        self._percent_cutoff = percent_cutoff
        self._output_path: str = output_path
        # Maps TID to a Dict of CPUs to total duration (ms) on that CPU.
        # E.g. For a TID of 1001 with 3 CPUs, this would be:
        #   {1001: {0: 1123.123, 1: 123123.123, 3: 1231.23}}
        self._tid_to_durations: Dict[int, Dict[int, float]] = {}
        self._breakdown: List[Dict[str, object]] = []
        self._tid_to_thread_name: Dict[int, str] = {}
        self._tid_to_process_name: Dict[int, str] = {}
        # Map of CPU to total duration used (ms).
        self._cpu_to_total_duration: Dict[int, float] = {}

    def _calculate_duration_per_cpu(
        self,
        cpu: int,
        records: List[trace_model.ContextSwitch],
    ) -> None:
        """
        Calculates the total duration for each thread, on a particular CPU.

        Uses a List of sorted ContextSwitch records to sum up the duration for each thread.
        It's possible that consecutive records do not have matching incoming_tid and outgoing_tid.
        """
        smallest_timestamp = self._timestamp_ms(records[0].start)
        largest_timestamp = self._timestamp_ms(records[-1].start)
        total_duration = largest_timestamp - smallest_timestamp
        self._cpu_to_total_duration[cpu] = total_duration

        for prev_record, curr_record in itertools.pairwise(records):
            # Check that the previous ContextSwitch's incoming_tid ("this thread is starting work
            # on this CPU") matches the current ContextSwitch's outgoing_tid ("this thread is being
            # switched away from"). If so, there is a duration to calculate. Otherwise, it means
            # maybe there is skipped data or something.
            if prev_record.tid != curr_record.outgoing_tid:
                # TODO(https://fxbug.dev/331458411): Record how often skipping happens.
                _LOGGER.info(
                    "Possibly missing a ContextSwitch record in trace."
                )
            # Purposely skip saving idle thread durations.
            elif prev_record.is_idle():
                continue
            else:
                start_ts = self._timestamp_ms(prev_record.start)
                stop_ts = self._timestamp_ms(curr_record.start)
                duration = stop_ts - start_ts
                assert duration >= 0
                if curr_record.outgoing_tid in self._tid_to_thread_name:
                    # Add duration to the total duration for that tid and CPU.
                    self._tid_to_durations.setdefault(
                        curr_record.outgoing_tid, {}
                    ).setdefault(cpu, 0)
                    self._tid_to_durations[curr_record.outgoing_tid][
                        cpu
                    ] += duration

    @staticmethod
    def _timestamp_ms(timestamp: trace_time.TimePoint) -> float:
        """
        Return timestamp in ms.
        """
        return timestamp.to_epoch_delta().to_milliseconds_f()

    def process_metrics(self) -> List[Dict[str, object]]:
        """
        Given TraceModel:
        - Iterates through all the SchedulingRecords and calculates the duration
        for each Process's Threads, and saves them by CPU.
        - Writes durations into free-form-metric JSON file.
        """
        # Map tids to names.
        for p in self._model.processes:
            for t in p.threads:
                self._tid_to_process_name[t.tid] = p.name
                self._tid_to_thread_name[t.tid] = t.name

        # Calculate durations for each CPU for each tid.
        for cpu, records in self._model.scheduling_records.items():
            self._calculate_duration_per_cpu(
                cpu,
                sorted(
                    (
                        r
                        for r in records
                        if isinstance(r, trace_model.ContextSwitch)
                    ),
                    key=lambda record: record.start,
                ),
            )

        # Calculate the percent of time the thread spent on this CPU,
        # compared to the total CPU duration.
        # If the percent spent is at or above our cutoff, add metric to
        # breakdown.
        for tid, breakdown in self._tid_to_durations.items():
            if tid in self._tid_to_thread_name:
                for cpu, duration in breakdown.items():
                    percent = round(
                        duration / self._cpu_to_total_duration[cpu] * 100, 3
                    )
                    if percent >= self._percent_cutoff:
                        metric = {
                            "process_name": self._tid_to_process_name[tid],
                            "thread_name": self._tid_to_thread_name[tid],
                            "cpu": cpu,
                            "percent": percent,
                            "duration": duration,
                        }
                        self._breakdown.append(metric)

        # Sort metrics by CPU (desc) and percent (desc).
        self._breakdown = sorted(
            self._breakdown,
            key=lambda m: (m["cpu"], m["percent"]),
            reverse=True,
        )
        return self._breakdown
