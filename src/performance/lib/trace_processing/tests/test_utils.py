#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utilities for trace model tests."""

import unittest

import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time

TEST_MODEL_BEGIN_TIME_IN_US: float = 697503138.0
TEST_MODEL_END_TIME_IN_US: float = 698607465.375


def get_test_model() -> trace_model.Model:
    read_event = trace_model.DurationEvent(
        duration=trace_time.TimeDelta.from_microseconds(698607461.7395687)
        - trace_time.TimeDelta.from_microseconds(697503138.9531089),
        parent=None,
        child_durations=[],
        child_flows=[],
        base=trace_model.Event(
            category="io",
            name="Read",
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503138.9531089)
            ),
            pid=7009,
            tid=7021,
            args={},
        ),
    )
    write_event = trace_model.DurationEvent(
        duration=trace_time.TimeDelta.from_microseconds(697868582.5994568)
        - trace_time.TimeDelta.from_microseconds(697778328.2160872),
        parent=None,
        child_durations=[],
        child_flows=[],
        base=trace_model.Event(
            category="io",
            name="Write",
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697778328.2160872)
            ),
            pid=7009,
            tid=7022,
            args={},
        ),
    )
    async_read_write_event = trace_model.AsyncEvent(
        id=43,
        duration=trace_time.TimeDelta.from_microseconds(698607461.0)
        - trace_time.TimeDelta.from_microseconds(TEST_MODEL_BEGIN_TIME_IN_US),
        base=trace_model.Event(
            category="io",
            name="AsyncReadWrite",
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(
                    int(TEST_MODEL_BEGIN_TIME_IN_US)
                )
            ),
            pid=7009,
            tid=7022,
            args={},
        ),
    )
    read_event2 = trace_model.DurationEvent(
        duration=trace_time.TimeDelta.from_microseconds(697868571.6018075)
        - trace_time.TimeDelta.from_microseconds(697868185.3588456),
        parent=None,
        child_durations=[],
        child_flows=[],
        base=trace_model.Event(
            category="io",
            name="Read",
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697868185.3588456)
            ),
            pid=7010,
            tid=7023,
            args={},
        ),
    )
    flow_start = trace_model.FlowEvent(
        id="0",
        phase=trace_model.FlowEventPhase.START,
        enclosing_duration=None,
        previous_flow=None,
        next_flow=None,
        base=trace_model.Event(
            category="io",
            name="ReadWriteFlow",
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503139.9531089)
            ),
            pid=7009,
            tid=7021,
            args={},
        ),
    )
    flow_step = trace_model.FlowEvent(
        id="0",
        phase=trace_model.FlowEventPhase.STEP,
        enclosing_duration=None,
        previous_flow=None,
        next_flow=None,
        base=trace_model.Event(
            category="io",
            name="ReadWriteFlow",
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697779328.2160872)
            ),
            pid=7009,
            tid=7022,
            args={},
        ),
    )
    flow_end = trace_model.FlowEvent(
        id="0",
        phase=trace_model.FlowEventPhase.END,
        enclosing_duration=None,
        previous_flow=None,
        next_flow=None,
        base=trace_model.Event(
            category="io",
            name="ReadWriteFlow",
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697868050.2160872)
            ),
            pid=7009,
            tid=7022,
            args={},
        ),
    )
    counter_event = trace_model.CounterEvent(
        id=None,
        base=trace_model.Event(
            category="system_metrics",
            name="cpu_usage",
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(
                    TEST_MODEL_END_TIME_IN_US
                )
            ),
            pid=7010,
            tid=7023,
            args={
                "average_cpu_percentage": 0.89349317793,
                "max_cpu_usage": 0.1234,
            },
        ),
    )
    instant_event = trace_model.InstantEvent(
        scope=trace_model.InstantEventScope.GLOBAL,
        base=trace_model.Event(
            category="log",
            name="log",
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(698607465.312)
            ),
            pid=7009,
            tid=7021,
            args={"message": "[INFO:trace_manager.cc(66)] Stopping trace"},
        ),
    )

    flow_start.enclosing_duration = read_event
    flow_start.previous_flow = None
    flow_start.next_flow = flow_step

    flow_step.enclosing_duration = write_event
    flow_step.previous_flow = flow_start
    flow_step.next_flow = flow_end

    flow_end.enclosing_duration = write_event
    flow_end.previous_flow = flow_step
    flow_end.next_flow = None

    read_event.child_flows = [flow_start]
    write_event.child_flows = [flow_step, flow_end]

    # No trace events for the following two threads.
    thread1036 = trace_model.Thread(tid=1036, name="memory-pressure-loop")
    thread5555 = trace_model.Thread(tid=5555, name="initial-thread")

    thread7021 = trace_model.Thread(
        tid=7021,
        name="tid: 7021",
        events=[read_event, flow_start, instant_event],
    )
    thread7022 = trace_model.Thread(
        tid=7022,
        name="initial-thread",
        events=[async_read_write_event, write_event, flow_step, flow_end],
    )
    thread7023 = trace_model.Thread(
        tid=7023, name="tid: 7023", events=[read_event2, counter_event]
    )

    process7009 = trace_model.Process(
        pid=7009,
        name="process_foo",
        threads=[thread1036, thread7021, thread7022],
    )
    process7010 = trace_model.Process(pid=7010, threads=[thread7023])
    process7011 = trace_model.Process(
        pid=7011, name="process-with-no-trace-events", threads=[thread5555]
    )

    model = trace_model.Model()
    model.processes = [process7009, process7010, process7011]
    model.scheduling_records[0] = [
        trace_model.Waking(
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503138)
            ),
            tid=7021,
            prio=3122,
            args={},
        ),
        trace_model.ContextSwitch(
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503138.9531089)
            ),
            incoming_tid=7022,
            outgoing_tid=7021,
            incoming_prio=3122,
            outgoing_prio=3122,
            outgoing_state=3,
            args={},
        ),
        trace_model.ContextSwitch(
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503139.9531089)
            ),
            incoming_tid=1036,
            outgoing_tid=7022,
            incoming_prio=-2147483648,
            outgoing_prio=3122,
            outgoing_state=3,
            args={},
        ),
        trace_model.ContextSwitch(
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(20)
            ),
            incoming_tid=1037,
            outgoing_tid=1036,
            incoming_prio=3122,
            outgoing_prio=3122,
            outgoing_state=3,
            args={},
        ),
        trace_model.ContextSwitch(
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(30)
            ),
            incoming_tid=5555,
            outgoing_tid=1037,
            incoming_prio=3122,
            outgoing_prio=3122,
            outgoing_state=3,
            args={},
        ),
    ]
    model.scheduling_records[1] = [
        trace_model.Waking(
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503138)
            ),
            tid=7021,
            prio=3122,
            args={},
        ),
        trace_model.ContextSwitch(
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503138.9531089)
            ),
            incoming_tid=7022,
            outgoing_tid=7021,
            incoming_prio=3122,
            outgoing_prio=3122,
            outgoing_state=3,
            args={},
        ),
        trace_model.ContextSwitch(
            start=trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503139.9531089)
            ),
            incoming_tid=1036,
            outgoing_tid=7022,
            incoming_prio=-2147483648,
            outgoing_prio=3122,
            outgoing_state=3,
            args={},
        ),
    ]

    return model


def assertEventsEqual(
    test: unittest.TestCase, a: trace_model.Event, b: trace_model.Event
) -> None:
    test.assertIs(type(a), type(b))

    # Check basic [trace_model.Event] fields.
    test.assertEqual(a.category, b.category)
    test.assertEqual(a.name, b.name)
    test.assertEqual(a.start, b.start)
    test.assertEqual(a.pid, b.pid)
    test.assertEqual(a.tid, b.tid)

    # The [args] field of an [trace_model.Event] should never be null.
    test.assertIsNotNone(a.args)
    test.assertIsNotNone(b.args)

    # Note: Rather than trying to handling the possibly complicated object
    # structure on each event here for equality, we just verify that their
    # key sets are equal.  This is safe, as this function is only used for
    # testing, rather than publicy exposed.
    test.assertEqual(len(a.args), len(b.args))
    test.assertEqual(set(a.args.keys()), b.args.keys())

    if isinstance(a, trace_model.InstantEvent) and isinstance(
        b, trace_model.InstantEvent
    ):
        test.assertEqual(a.scope, b.scope)
    elif isinstance(a, trace_model.CounterEvent) and isinstance(
        b, trace_model.CounterEvent
    ):
        test.assertEqual(a.id, b.id)
    elif isinstance(a, trace_model.DurationEvent) and isinstance(
        b, trace_model.DurationEvent
    ):
        test.assertAlmostEqual(
            a.duration.to_microseconds(), b.duration.to_microseconds()
        )
        test.assertEqual(a.parent is None, b.parent is None)
        test.assertEqual(len(a.child_durations), len(b.child_durations))
        test.assertEqual(len(a.child_flows), len(b.child_flows))
    elif isinstance(a, trace_model.AsyncEvent) and isinstance(
        b, trace_model.AsyncEvent
    ):
        test.assertEqual(a.id, b.id)
        test.assertAlmostEqual(
            a.duration.to_microseconds(), b.duration.to_microseconds()
        )
    elif isinstance(a, trace_model.FlowEvent) and isinstance(
        b, trace_model.FlowEvent
    ):
        test.assertEqual(a.id, b.id)
        test.assertEqual(a.phase, b.phase)
        test.assertIsNotNone(a.enclosing_duration)
        test.assertIsNotNone(b.enclosing_duration)
        test.assertEqual(a.previous_flow is None, b.previous_flow is None)
        test.assertEqual(a.next_flow is None, b.next_flow is None)
    else:
        test.fail(f"event {a} and event {b} were unrecognized types")


def assertThreadsEqual(
    test: unittest.TestCase, a: trace_model.Thread, b: trace_model.Thread
) -> None:
    test.assertEqual(
        a.tid, b.tid, f"Error, thread tids did match: {a.tid} vs {b.tid}"
    )
    test.assertEqual(
        a.name,
        b.name,
        f"Error, thread names (tid={a.tid}) did not match: {a.name} vs "
        f"{b.name}",
    )
    test.assertEqual(
        len(a.events),
        len(b.events),
        f"Error, thread (tid={a.tid}, name={a.name}) events lengths did "
        f"not match: {len(a.events)} vs {len(b.events)}",
    )
    for a_event, b_event in zip(a.events, b.events):
        assertEventsEqual(test, a_event, b_event)


def assertProcessesEqual(
    test: unittest.TestCase, a: trace_model.Process, b: trace_model.Process
) -> None:
    test.assertEqual(
        a.pid, b.pid, f"Error, process pids did match: {a.pid} vs {b.pid}"
    )
    test.assertEqual(
        a.name,
        b.name,
        f"Error, process (pid={a.pid}) names did not match: {a.name} vs "
        f"{b.name}",
    )
    test.assertEqual(
        len(a.threads),
        len(b.threads),
        f"Error, process (pid={a.pid}, name={a.name}) threads lengths did "
        f"not match: {len(a.threads)} vs {len(b.threads)}",
    )
    for a_thread, b_thread in zip(a.threads, b.threads):
        assertThreadsEqual(test, a_thread, b_thread)


def assertSchedulingRecordEqual(
    test: unittest.TestCase,
    a: trace_model.SchedulingRecord,
    b: trace_model.SchedulingRecord,
) -> None:
    test.assertIs(type(a), type(b))

    # Check basic [trace_model.SchedulingRecord] fields.
    test.assertEqual(a.start, b.start)
    test.assertEqual(a.tid, b.tid)
    test.assertEqual(a.prio, b.prio)

    # Note: Like for events, rather than trying to handling the possibly complicated object
    # structure on each event here for equality, we just verify that their key sets are equal.
    test.assertEqual(len(a.args), len(b.args))
    test.assertEqual(set(a.args.keys()), b.args.keys())

    if isinstance(a, trace_model.ContextSwitch) and isinstance(
        b, trace_model.ContextSwitch
    ):
        test.assertEqual(a.tid, b.tid)
        test.assertEqual(a.outgoing_tid, b.outgoing_tid)
        test.assertEqual(a.outgoing_prio, b.outgoing_prio)
        test.assertEqual(a.outgoing_state, b.outgoing_state)


def assertModelsEqual(
    test: unittest.TestCase, a: trace_model.Model, b: trace_model.Model
) -> None:
    test.assertEqual(
        len(a.processes),
        len(b.processes),
        f"Error, model processes lengths did not match: {len(a.processes)} "
        f"vs {len(b.processes)}",
    )
    for a_process, b_process in zip(a.processes, b.processes):
        assertProcessesEqual(test, a_process, b_process)

    test.assertEqual(
        len(a.scheduling_records),
        len(b.scheduling_records),
        f"Error, model scheduling record lengths did not match: {len(a.scheduling_records)} "
        f"vs {len(b.scheduling_records)}",
    )

    for cpu, records in a.scheduling_records.items():
        for a_record, b_record in zip(
            a.scheduling_records[cpu], b.scheduling_records[cpu]
        ):
            assertSchedulingRecordEqual(test, a_record, b_record)
