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


class CpuBreakdownMetricsProcessor:
    """
    Breaks down CPU metrics into a free-form metrics format, and
    writes the metrics to JSON.
    """

    def __init__(self, model: trace_model.Model) -> None:
        self._model: trace_model.Model = model
        # Maps TID to a Dict of CPUs to total duration (ms) on that CPU.
        # E.g. For a TID of 1001 with 3 CPUs, this would be:
        #   {1001: {0: 1123.123, 1: 123123.123, 3: 1231.23}}
        self._tid_to_durations: Dict[int, Dict[int, float]] = {}
        # Currently we just print this but we will output a JSON soon.
        self._breakdown: Dict[str, Dict[int, float]] = {}
        # Name composed of Process name and Thread name.
        self._tid_to_name: Dict[int, str] = {}

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
                if curr_record.outgoing_tid in self._tid_to_name:
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

    def process_metrics(self) -> Dict[str, Dict[int, float]]:
        """
        Given TraceModel:
        - Iterates through all the SchedulingRecords and calculates the duration
        for each Process's Threads, and saves them by CPU.
        - Writes durations into free-form-metric JSON file. (TODO: https://fxbug.dev/331457527)
        """
        # Map tids to names.
        for p in self._model.processes:
            for t in p.threads:
                self._tid_to_name[t.tid] = "%s: %s" % (p.name, t.name)

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

        # Return CPU breakdown.
        # TODO: https://fxbug.dev/331457527 format this better.
        for tid in self._tid_to_durations:
            self._breakdown[self._tid_to_name[tid]] = self._tid_to_durations[
                tid
            ]
        return self._breakdown
