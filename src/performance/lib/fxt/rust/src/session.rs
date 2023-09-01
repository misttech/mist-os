// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    error::ParseWarning,
    init::{InitRecord, Ticks},
    metadata::{
        MetadataRecord, Provider, ProviderEventMetadataRecord, ProviderInfoMetadataRecord,
        ProviderSectionMetadataRecord, TraceInfoMetadataRecord,
    },
    string::{StringRecord, StringRef},
    thread::{ProcessKoid, ProcessRef, ThreadKoid, ThreadRecord, ThreadRef},
    ParseError, ParsedWithOriginalBytes, RawTraceRecord, TraceRecord,
};
use flyweights::FlyStr;
use futures::{AsyncRead, AsyncReadExt, SinkExt, Stream};
use std::{
    collections::BTreeMap,
    marker::Unpin,
    num::{NonZeroU16, NonZeroU8},
};

pub fn parse_full_session<'a>(
    buf: &'a [u8],
) -> Result<(Vec<TraceRecord>, Vec<ParseWarning>), ParseError> {
    let mut parser = SessionParser::new(std::io::Cursor::new(buf));
    let mut records = vec![];
    while let Some(record) = parser.next() {
        records.push(record?);
    }
    Ok((records, parser.warnings().to_owned()))
}

#[derive(Debug, PartialEq)]
pub struct SessionParser<R> {
    buffer: Vec<u8>,
    reader: R,
    resolver: ResolveCtx,
    reader_is_eof: bool,
    have_seen_magic_number: bool,
}

impl<R: std::io::Read> SessionParser<R> {
    pub fn new(reader: R) -> Self {
        Self {
            buffer: vec![],
            reader,
            resolver: ResolveCtx::new(),
            reader_is_eof: false,
            have_seen_magic_number: false,
        }
    }
}

impl<R> SessionParser<R> {
    pub fn warnings(&self) -> &[ParseWarning] {
        self.resolver.warnings()
    }

    fn parse_next(&mut self) -> ParseOutcome {
        match RawTraceRecord::parse(&self.buffer) {
            Ok((rem, ParsedWithOriginalBytes { parsed: raw_record, .. })) => {
                // Make sure the first record we encounter is the magic number record.
                if raw_record.is_magic_number() {
                    self.have_seen_magic_number = true;
                } else {
                    if !self.have_seen_magic_number {
                        return ParseOutcome::Error(ParseError::MissingMagicNumber);
                    }
                }

                // Resolve the record to end our borrow on the buffer before rotating it.
                let resolve_res = TraceRecord::resolve(&mut self.resolver, raw_record);

                // Update our buffer based on how much was unused to parse this record.
                let unused_len = rem.len();
                let parsed_len = self.buffer.len() - unused_len;
                self.buffer.copy_within(parsed_len.., 0);
                self.buffer.truncate(unused_len);

                match resolve_res {
                    // Updated resolution state but don't have any logical records to return,
                    // try again.
                    Ok(None) => ParseOutcome::Continue,
                    Ok(Some(resolved)) => ParseOutcome::GotRecord(resolved),
                    Err(e) => ParseOutcome::Error(e),
                }
            }
            Err(nom::Err::Error(e) | nom::Err::Failure(e)) => ParseOutcome::Error(e),
            Err(nom::Err::Incomplete(needed)) => {
                ParseOutcome::NeedMoreBytes(match needed {
                    // Fall back to asking for the max trace record size if we don't know
                    // how much we want.
                    nom::Needed::Unknown => 32768,
                    nom::Needed::Size(n) => n,
                })
            }
        }
    }
}

enum ParseOutcome {
    GotRecord(TraceRecord),
    Continue,
    Error(ParseError),
    NeedMoreBytes(usize),
}

// We use a macro here because it's difficult to abstract over sync vs. async for read callbacks.
macro_rules! fill_buffer {
    ($self:ident, $original_len:ident, $needed:ident, $bytes_read:expr) => {{
        if $self.reader_is_eof {
            // We already reached the end of the reader and still failed to parse, so
            // this iterator is done.
            return None;
        } else {
            let $original_len = $self.buffer.len();
            $self.buffer.resize($original_len + $needed, 0);
            let bytes_read = $bytes_read;
            if bytes_read == 0 {
                $self.reader_is_eof = true;
            }
            $self.buffer.truncate($original_len + bytes_read);
        }
    }};
}

impl<R: std::io::Read> Iterator for SessionParser<R> {
    type Item = Result<TraceRecord, ParseError>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.parse_next() {
                ParseOutcome::GotRecord(r) => return Some(Ok(r)),
                ParseOutcome::Error(e) => return Some(Err(e)),
                ParseOutcome::Continue => continue,
                ParseOutcome::NeedMoreBytes(needed) => {
                    fill_buffer!(
                        self,
                        original_len,
                        needed,
                        match self.reader.read(&mut self.buffer[original_len..]) {
                            Ok(b) => b,
                            Err(e) => return Some(Err(ParseError::Io(e))),
                        }
                    );
                }
            }
        }
    }
}

impl<R: AsyncRead + Send + Unpin + 'static> SessionParser<R> {
    pub fn new_async(
        reader: R,
    ) -> (impl Stream<Item = Result<TraceRecord, ParseError>>, fuchsia_async::Task<Vec<ParseWarning>>)
    {
        // Bounce the records through a channel to avoid a Stream impl and all the pinning.
        let (mut send, recv) = futures::channel::mpsc::channel(1);
        let pump_task = fuchsia_async::Task::spawn(async move {
            let mut parser = Self {
                buffer: vec![],
                reader,
                resolver: ResolveCtx::new(),
                reader_is_eof: false,
                have_seen_magic_number: false,
            };

            while let Some(next) = parser.next_async().await {
                if send.send(next).await.is_err() {
                    // The listener has disconnected, don't need to keep parsing.
                    break;
                }
            }

            parser.warnings().to_owned()
        });

        (recv, pump_task)
    }

    pub async fn next_async(&mut self) -> Option<Result<TraceRecord, ParseError>> {
        loop {
            match self.parse_next() {
                ParseOutcome::GotRecord(r) => return Some(Ok(r)),
                ParseOutcome::Error(e) => return Some(Err(e)),
                ParseOutcome::Continue => continue,
                ParseOutcome::NeedMoreBytes(needed) => {
                    fill_buffer!(
                        self,
                        original_len,
                        needed,
                        match self.reader.read(&mut self.buffer[original_len..]).await {
                            Ok(b) => b,
                            Err(e) => return Some(Err(ParseError::Io(e))),
                        }
                    );
                }
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct ResolveCtx {
    ticks_per_second: u64,
    current_provider: Option<Provider>,
    providers: BTreeMap<u32, FlyStr>,
    strings: BTreeMap<NonZeroU16, FlyStr>,
    threads: BTreeMap<NonZeroU8, (ProcessKoid, ThreadKoid)>,
    warnings: Vec<ParseWarning>,
}

impl ResolveCtx {
    pub fn new() -> Self {
        Self {
            ticks_per_second: 1,
            current_provider: None,
            providers: Default::default(),
            strings: Default::default(),
            threads: Default::default(),
            warnings: Default::default(),
        }
    }

    pub fn add_warning(&mut self, warning: ParseWarning) {
        self.warnings.push(warning);
    }

    pub fn warnings(&self) -> &[ParseWarning] {
        &self.warnings
    }

    pub fn current_provider(&self) -> Option<Provider> {
        self.current_provider.clone()
    }

    pub fn get_provider(&mut self, id: u32) -> Result<Provider, ParseError> {
        let name = if let Some(name) = self.providers.get(&id).cloned() {
            name
        } else {
            self.add_warning(ParseWarning::UnknownProviderId(id));
            "<unknown>".into()
        };

        Ok(Provider { id, name })
    }

    pub fn on_metadata_record(
        &mut self,
        m: MetadataRecord,
    ) -> Result<Option<TraceRecord>, ParseError> {
        Ok(match m {
            // No action to take on the magic number.
            MetadataRecord::TraceInfo(TraceInfoMetadataRecord::MagicNumber) => None,

            MetadataRecord::ProviderInfo(ProviderInfoMetadataRecord { provider_id, name }) => {
                self.providers.insert(provider_id, name.clone());
                self.current_provider = Some(Provider { id: provider_id, name: name });
                None
            }
            MetadataRecord::ProviderSection(ProviderSectionMetadataRecord { provider_id }) => {
                let new_provider = self.get_provider(provider_id)?;
                self.current_provider = Some(new_provider);
                None
            }
            MetadataRecord::ProviderEvent(ProviderEventMetadataRecord { provider_id, event }) => {
                Some(TraceRecord::ProviderEvent {
                    provider: self.get_provider(provider_id)?,
                    event,
                })
            }
            MetadataRecord::Unknown { raw_type } => {
                self.add_warning(ParseWarning::UnknownMetadataRecordType(raw_type));
                None
            }
        })
    }

    pub fn on_init_record(&mut self, InitRecord { ticks_per_second }: InitRecord) {
        self.ticks_per_second = ticks_per_second;
    }

    pub fn on_string_record(&mut self, s: StringRecord<'_>) {
        if let Some(idx) = NonZeroU16::new(s.index) {
            self.strings.insert(idx, s.value.into());
        } else {
            self.add_warning(ParseWarning::RecordForZeroStringId);
        }
    }

    pub fn on_thread_record(&mut self, t: ThreadRecord) {
        self.threads.insert(t.index, (t.process_koid, t.thread_koid));
    }

    pub fn resolve_str(&mut self, s: StringRef<'_>) -> FlyStr {
        match s {
            StringRef::Empty => FlyStr::default(),
            StringRef::Inline(inline) => FlyStr::from(inline),
            StringRef::Index(id) => {
                if let Some(s) = self.strings.get(&id).cloned() {
                    s
                } else {
                    self.add_warning(ParseWarning::UnknownStringId(id));
                    "<unknown>".into()
                }
            }
        }
    }

    pub fn resolve_process(&mut self, p: ProcessRef) -> ProcessKoid {
        match p {
            ProcessRef::Index(id) => {
                if let Some(process) = self.threads.get(&id).map(|(process, _thread)| *process) {
                    process
                } else {
                    self.add_warning(ParseWarning::UnknownProcessRef(p));
                    ProcessKoid(std::u64::MAX)
                }
            }
            ProcessRef::Inline(inline) => inline,
        }
    }

    pub fn resolve_thread(&mut self, t: ThreadRef) -> ThreadKoid {
        match t {
            ThreadRef::Index(id) => {
                if let Some(thread) = self.threads.get(&id).map(|(_process, thread)| *thread) {
                    thread
                } else {
                    self.warnings.push(ParseWarning::UnknownThreadRef(t));
                    ThreadKoid(std::u64::MAX)
                }
            }
            ThreadRef::Inline(inline) => inline,
        }
    }

    pub fn resolve_ticks(&self, t: Ticks) -> i64 {
        t.scale(self.ticks_per_second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        event::{EventPayload, EventRecord},
        scheduling::{LegacyContextSwitchEvent, SchedulingRecord, ThreadState},
    };
    use futures::{SinkExt, StreamExt, TryStreamExt};

    static SIMPLE_TRACE_FXT: &[u8] =
        include_bytes!("../../../../trace2json/test_data/simple_trace.fxt");

    #[test]
    fn test_parse_full_session() {
        let session = parse_full_session(SIMPLE_TRACE_FXT).unwrap();
        assert_eq!(session, expected_simple_trace_records());
    }

    #[fuchsia::test]
    async fn test_async_parse() {
        let (mut send_chunks, recv_chunks) = futures::channel::mpsc::unbounded();

        let parse_trace_session = fuchsia_async::Task::spawn(async move {
            let (records, parse_task) = SessionParser::new_async(recv_chunks.into_async_read());
            let records = records.map(|res| res.unwrap()).collect::<Vec<_>>().await;
            (records, parse_task.await)
        });

        // Send tiny chunks to shake out any incorrect streaming parser code.
        for chunk in SIMPLE_TRACE_FXT.chunks(1) {
            send_chunks.send(Ok(chunk)).await.unwrap();
        }
        drop(send_chunks);

        assert_eq!(parse_trace_session.await, expected_simple_trace_records());
    }

    #[fuchsia::test]
    fn session_with_unknown_record_in_middle() {
        let mut session = vec![];

        // Add the magic record from the simple trace before we add our bogus record so we don't
        // error on an invalid first record.
        session.extend(&SIMPLE_TRACE_FXT[..8]);

        // Add our bogus record with an unknown type, expecting to skip over it.
        let mut header = crate::BaseTraceHeader::empty();
        header.set_raw_type(14); // not currently a valid ordinal
        session.extend(
            crate::testing::FxtBuilder::new(header).atom(&(0u8..27u8).collect::<Vec<u8>>()).build(),
        );

        // Add the rest of the simple trace.
        session.extend(&SIMPLE_TRACE_FXT[8..]);

        let (observed_parsed, observed_warnings) = parse_full_session(&session).unwrap();
        let (expected_parsed, expected_warnings) =
            (expected_simple_trace_records().0, vec![ParseWarning::UnknownTraceRecordType(14)]);
        assert_eq!(observed_parsed, expected_parsed);
        assert_eq!(observed_warnings, expected_warnings);
    }

    #[fuchsia::test]
    fn session_with_incomplete_trailing_record() {
        use crate::string::STRING_REF_INLINE_BIT;

        let mut session = SIMPLE_TRACE_FXT.to_vec();

        // Make a 2 word header with some arbitrary values.
        let category = "test_category";
        let name = "test_instant";
        let mut header = crate::event::EventHeader::empty();
        header.set_category_ref(category.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_event_type(crate::event::INSTANT_EVENT_TYPE);

        let mut final_record = crate::testing::FxtBuilder::new(header)
            .atom(2048u64.to_le_bytes()) // timestamp
            .atom(512u64.to_le_bytes()) // process
            .atom(513u64.to_le_bytes()) // thread
            .atom(category)
            .atom(name)
            .build();
        let byte_to_make_valid = final_record.pop().unwrap();

        for byte in final_record {
            session.push(byte);
            assert_eq!(
                parse_full_session(&session).expect("should parse without final incomplete record"),
                expected_simple_trace_records(),
            );
        }

        let (mut expected_with_final_record, expected_warnings) = expected_simple_trace_records();
        expected_with_final_record.push(TraceRecord::Event(EventRecord {
            provider: Some(Provider { id: 1, name: "test_provider".into() }),
            timestamp: 85333,
            process: ProcessKoid(512),
            thread: ThreadKoid(513),
            category: category.into(),
            name: name.into(),
            args: vec![],
            payload: EventPayload::Instant,
        }));

        session.push(byte_to_make_valid);
        assert_eq!(
            parse_full_session(&session).unwrap(),
            (expected_with_final_record, expected_warnings)
        );
    }

    fn expected_simple_trace_records() -> (Vec<TraceRecord>, Vec<ParseWarning>) {
        (
            vec![
                TraceRecord::Scheduling(SchedulingRecord::LegacyContextSwitch(
                    LegacyContextSwitchEvent {
                        provider: Some(Provider { id: 1, name: "test_provider".into() }),
                        timestamp: 41,
                        cpu_id: 0,
                        outgoing_thread_state: ThreadState::Suspended,
                        outgoing_process: ProcessKoid(4660),
                        outgoing_thread: ThreadKoid(17185),
                        outgoing_thread_priority: 0,
                        incoming_process: ProcessKoid(1000),
                        incoming_thread: ThreadKoid(1001),
                        incoming_thread_priority: 20,
                    },
                )),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 0,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "begin_end_ref".into(),
                    args: vec![],
                    payload: EventPayload::DurationBegin,
                }),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 110000000,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "complete_inline".into(),
                    args: vec![],
                    payload: EventPayload::DurationComplete { end_timestamp: 150000000 },
                }),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 200000000,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "begin_end_inline".into(),
                    args: vec![],
                    payload: EventPayload::DurationBegin,
                }),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 450000000,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "begin_end_inline".into(),
                    args: vec![],
                    payload: EventPayload::DurationEnd,
                }),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 100000000,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "complete_ref".into(),
                    args: vec![],
                    payload: EventPayload::DurationComplete { end_timestamp: 500000000 },
                }),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 500000208,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "async".into(),
                    args: vec![],
                    payload: EventPayload::AsyncBegin { id: 1 },
                }),
                TraceRecord::Scheduling(SchedulingRecord::LegacyContextSwitch(
                    LegacyContextSwitchEvent {
                        provider: Some(Provider { id: 1, name: "test_provider".into() }),
                        timestamp: 500000416,
                        cpu_id: 0,
                        outgoing_thread_state: ThreadState::Suspended,
                        outgoing_process: ProcessKoid(1000),
                        outgoing_thread: ThreadKoid(1001),
                        outgoing_thread_priority: 20,
                        incoming_process: ProcessKoid(1000),
                        incoming_thread: ThreadKoid(1002),
                        incoming_thread_priority: 20,
                    },
                )),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 500000458,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1002),
                    category: "test".into(),
                    name: "complete_ref".into(),
                    args: vec![],
                    payload: EventPayload::DurationComplete { end_timestamp: 600000000 },
                }),
                TraceRecord::Scheduling(SchedulingRecord::LegacyContextSwitch(
                    LegacyContextSwitchEvent {
                        provider: Some(Provider { id: 1, name: "test_provider".into() }),
                        timestamp: 600010666,
                        cpu_id: 0,
                        outgoing_thread_state: ThreadState::Suspended,
                        outgoing_process: ProcessKoid(1000),
                        outgoing_thread: ThreadKoid(1002),
                        outgoing_thread_priority: 20,
                        incoming_process: ProcessKoid(1000),
                        incoming_thread: ThreadKoid(1001),
                        incoming_thread_priority: 20,
                    },
                )),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 600016000,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "async".into(),
                    args: vec![],
                    payload: EventPayload::AsyncEnd { id: 1 },
                }),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 630000000,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "begin_end_ref".into(),
                    args: vec![],
                    payload: EventPayload::DurationBegin,
                }),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 950000000,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "begin_end_ref".into(),
                    args: vec![],
                    payload: EventPayload::DurationEnd,
                }),
                TraceRecord::Event(EventRecord {
                    provider: Some(Provider { id: 1, name: "test_provider".into() }),
                    timestamp: 1000000000,
                    process: ProcessKoid(1000),
                    thread: ThreadKoid(1001),
                    category: "test".into(),
                    name: "begin_end_ref".into(),
                    args: vec![],
                    payload: EventPayload::DurationEnd,
                }),
                TraceRecord::Scheduling(SchedulingRecord::LegacyContextSwitch(
                    LegacyContextSwitchEvent {
                        provider: Some(Provider { id: 1, name: "test_provider".into() }),
                        timestamp: 1000000666,
                        cpu_id: 0,
                        outgoing_thread_state: ThreadState::Suspended,
                        outgoing_process: ProcessKoid(1000),
                        outgoing_thread: ThreadKoid(1001),
                        outgoing_thread_priority: 20,
                        incoming_process: ProcessKoid(4660),
                        incoming_thread: ThreadKoid(17185),
                        incoming_thread_priority: 0,
                    },
                )),
            ],
            // There should be no warnings produced from these tests.
            vec![],
        )
    }
}
