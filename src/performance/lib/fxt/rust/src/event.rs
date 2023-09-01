// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    args::{Arg, RawArg},
    init::Ticks,
    session::ResolveCtx,
    string::StringRef,
    thread::{ProcessKoid, ProcessRef, ThreadKoid, ThreadRef},
    trace_header, ParseResult, Provider, EVENT_RECORD_TYPE,
};
use flyweights::FlyStr;
use nom::number::complete::le_u64;

pub(crate) const INSTANT_EVENT_TYPE: u8 = 0;
pub(crate) const COUNTER_EVENT_TYPE: u8 = 1;
pub(crate) const DURATION_BEGIN_EVENT_TYPE: u8 = 2;
pub(crate) const DURATION_END_EVENT_TYPE: u8 = 3;
pub(crate) const DURATION_COMPLETE_EVENT_TYPE: u8 = 4;
pub(crate) const ASYNC_BEGIN_EVENT_TYPE: u8 = 5;
pub(crate) const ASYNC_INSTANT_EVENT_TYPE: u8 = 6;
pub(crate) const ASYNC_END_EVENT_TYPE: u8 = 7;
pub(crate) const FLOW_BEGIN_EVENT_TYPE: u8 = 8;
pub(crate) const FLOW_STEP_EVENT_TYPE: u8 = 9;
pub(crate) const FLOW_END_EVENT_TYPE: u8 = 10;

#[derive(Clone, Debug, PartialEq)]
pub struct EventRecord {
    pub provider: Option<Provider>,
    pub timestamp: i64,
    pub process: ProcessKoid,
    pub thread: ThreadKoid,
    pub category: FlyStr,
    pub name: FlyStr,
    pub args: Vec<Arg>,
    pub payload: EventPayload<i64>,
}

impl EventRecord {
    pub(super) fn resolve(ctx: &mut ResolveCtx, raw: RawEventRecord<'_>) -> Self {
        Self {
            provider: ctx.current_provider(),
            timestamp: ctx.resolve_ticks(raw.ticks),
            process: ctx.resolve_process(raw.process),
            thread: ctx.resolve_thread(raw.thread),
            category: ctx.resolve_str(raw.category),
            name: ctx.resolve_str(raw.name),
            args: Arg::resolve_n(ctx, raw.args),
            payload: raw.payload.resolve(ctx),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct RawEventRecord<'a> {
    ticks: Ticks,
    process: ProcessRef,
    thread: ThreadRef,
    category: StringRef<'a>,
    name: StringRef<'a>,
    args: Vec<RawArg<'a>>,
    payload: EventPayload<Ticks>,
}

impl<'a> RawEventRecord<'a> {
    pub(super) fn parse(buf: &'a [u8]) -> ParseResult<'a, Self> {
        let (buf, header) = EventHeader::parse(buf)?;
        let (rem, payload) = header.take_payload(buf)?;
        let (payload, ticks) = Ticks::parse(payload)?;
        let (payload, process) = ProcessRef::parse(header.thread_ref(), payload)?;
        let (payload, thread) = ThreadRef::parse(header.thread_ref(), payload)?;
        let (payload, category) = StringRef::parse(header.category_ref(), payload)?;
        let (payload, name) = StringRef::parse(header.name_ref(), payload)?;
        let (payload, args) = RawArg::parse_n(header.num_args(), payload)?;

        // Some trace events attach an undocumented "scope" word on instant events for chrome trace
        // viewer compatibility that we don't need to return, so don't use all_consuming here.
        let (_empty, payload) = EventPayload::parse(header.event_type(), payload)?;
        Ok((rem, Self { ticks, process, thread, category, name, args, payload }))
    }
}

trace_header! {
    EventHeader (EVENT_RECORD_TYPE) {
        u8, event_type: 16, 19;
        u8, num_args: 20, 23;
        u8, thread_ref: 24, 31;
        u16, category_ref: 32, 47;
        u16, name_ref: 48, 63;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventPayload<Time> {
    Instant,
    Counter { id: u64 },
    DurationBegin,
    DurationEnd,
    DurationComplete { end_timestamp: Time },
    AsyncBegin { id: u64 },
    AsyncInstant { id: u64 },
    AsyncEnd { id: u64 },
    FlowBegin { id: u64 },
    FlowStep { id: u64 },
    FlowEnd { id: u64 },
    Unknown { raw_type: u8, bytes: Vec<u8> },
}

impl EventPayload<Ticks> {
    pub(crate) fn resolve(self, ctx: &ResolveCtx) -> EventPayload<i64> {
        match self {
            EventPayload::Instant => EventPayload::Instant,
            EventPayload::Counter { id } => EventPayload::Counter { id },
            EventPayload::DurationBegin => EventPayload::DurationBegin,
            EventPayload::DurationEnd => EventPayload::DurationEnd,
            EventPayload::DurationComplete { end_timestamp } => {
                EventPayload::DurationComplete { end_timestamp: ctx.resolve_ticks(end_timestamp) }
            }
            EventPayload::AsyncBegin { id } => EventPayload::AsyncBegin { id },
            EventPayload::AsyncInstant { id } => EventPayload::AsyncInstant { id },
            EventPayload::AsyncEnd { id } => EventPayload::AsyncEnd { id },
            EventPayload::FlowBegin { id } => EventPayload::FlowBegin { id },
            EventPayload::FlowStep { id } => EventPayload::FlowStep { id },
            EventPayload::FlowEnd { id } => EventPayload::FlowEnd { id },
            EventPayload::Unknown { raw_type, bytes } => EventPayload::Unknown { raw_type, bytes },
        }
    }
}

impl EventPayload<Ticks> {
    fn parse(event_type: u8, buf: &[u8]) -> ParseResult<'_, Self> {
        use nom::combinator::map;
        match event_type {
            INSTANT_EVENT_TYPE => Ok((buf, EventPayload::Instant)),
            COUNTER_EVENT_TYPE => map(le_u64, |id| EventPayload::Counter { id })(buf),
            DURATION_BEGIN_EVENT_TYPE => Ok((buf, EventPayload::DurationBegin)),
            DURATION_END_EVENT_TYPE => Ok((buf, EventPayload::DurationEnd)),
            DURATION_COMPLETE_EVENT_TYPE => {
                map(Ticks::parse, |end_timestamp| EventPayload::DurationComplete { end_timestamp })(
                    buf,
                )
            }
            ASYNC_BEGIN_EVENT_TYPE => map(le_u64, |id| EventPayload::AsyncBegin { id })(buf),
            ASYNC_INSTANT_EVENT_TYPE => map(le_u64, |id| EventPayload::AsyncInstant { id })(buf),
            ASYNC_END_EVENT_TYPE => map(le_u64, |id| EventPayload::AsyncEnd { id })(buf),
            FLOW_BEGIN_EVENT_TYPE => map(le_u64, |id| EventPayload::FlowBegin { id })(buf),
            FLOW_STEP_EVENT_TYPE => map(le_u64, |id| EventPayload::FlowStep { id })(buf),
            FLOW_END_EVENT_TYPE => map(le_u64, |id| EventPayload::FlowEnd { id })(buf),
            unknown => {
                Ok((&[][..], EventPayload::Unknown { raw_type: unknown, bytes: buf.to_vec() }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{string::STRING_REF_INLINE_BIT, testing::FxtBuilder, RawTraceRecord};
    use std::num::{NonZeroU16, NonZeroU8};

    #[test]
    fn event_no_args() {
        let mut header = EventHeader::empty();
        header.set_thread_ref(11);
        header.set_category_ref(27);
        header.set_name_ref(93);
        header.set_num_args(0);
        header.set_event_type(INSTANT_EVENT_TYPE);

        assert_parses_to_record!(
            FxtBuilder::new(header).atom(2048u64.to_le_bytes()).build(),
            RawTraceRecord::Event(RawEventRecord {
                ticks: Ticks(2048),
                process: ProcessRef::Index(NonZeroU8::new(11).unwrap()),
                thread: ThreadRef::Index(NonZeroU8::new(11).unwrap()),
                category: StringRef::Index(NonZeroU16::new(27).unwrap()),
                name: StringRef::Index(NonZeroU16::new(93).unwrap()),
                args: vec![],
                payload: EventPayload::Instant,
            }),
        );
    }

    #[test]
    fn event_with_args() {
        let mut header = EventHeader::empty();
        header.set_event_type(DURATION_COMPLETE_EVENT_TYPE);
        header.set_category_ref("event_category".len() as u16 | STRING_REF_INLINE_BIT);
        header.set_name_ref("event_name".len() as u16 | STRING_REF_INLINE_BIT);
        header.set_num_args(2);

        let first_arg_name = "arg1";
        let first_arg_value = "val1";
        let mut first_arg_header = crate::args::StringHeader::empty();
        first_arg_header.set_name_ref(first_arg_name.len() as u16 | STRING_REF_INLINE_BIT);
        first_arg_header.set_value_ref(first_arg_value.len() as u16 | STRING_REF_INLINE_BIT);

        let second_arg_name = "arg2";
        let mut second_arg_header = crate::args::BaseArgHeader::empty();
        second_arg_header.set_raw_type(crate::args::PTR_ARG_TYPE);
        second_arg_header.set_name_ref(second_arg_name.len() as u16 | STRING_REF_INLINE_BIT);

        assert_parses_to_record!(
            FxtBuilder::new(header)
                // begin ticks
                .atom(2048u64.to_le_bytes())
                // process
                .atom(345u64.to_le_bytes())
                // thread
                .atom(678u64.to_le_bytes())
                // category
                .atom("event_category")
                // name
                .atom("event_name")
                // first arg
                .atom(
                    FxtBuilder::new(first_arg_header)
                        .atom(first_arg_name)
                        .atom(first_arg_value)
                        .build()
                )
                // second arg
                .atom(
                    FxtBuilder::new(second_arg_header)
                        .atom(second_arg_name)
                        .atom(123456u64.to_le_bytes())
                        .build()
                )
                // end ticks
                .atom(4096u64.to_le_bytes())
                .build(),
            RawTraceRecord::Event(RawEventRecord {
                ticks: Ticks(2048),
                process: ProcessRef::Inline(ProcessKoid(345)),
                thread: ThreadRef::Inline(ThreadKoid(678)),
                category: StringRef::Inline("event_category"),
                name: StringRef::Inline("event_name"),
                args: vec![
                    RawArg {
                        name: StringRef::Inline(first_arg_name),
                        value: crate::args::RawArgValue::String(StringRef::Inline(first_arg_value)),
                    },
                    RawArg {
                        name: StringRef::Inline(second_arg_name),
                        value: crate::args::RawArgValue::Pointer(123456),
                    }
                ],
                payload: EventPayload::DurationComplete { end_timestamp: Ticks(4096) },
            }),
        );
    }
}
