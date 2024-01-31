// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::CurrentTask,
    vfs::{
        buffers::InputBuffer, fileops_impl_delegate_read_and_seek, DynamicFile, DynamicFileBuf,
        DynamicFileSource, FileObject, FileOps, FsNodeOps, SimpleFileNode,
    },
};
use starnix_logging::trace_category_atrace;
use starnix_sync::{Locked, WriteOps};
use starnix_uapi::errors::Errno;

use std::{collections::HashMap, sync::Mutex};

use fuchsia_trace::{ArgValue, Scope, TraceCategoryContext};
use fuchsia_zircon as zx;
use fuchsia_zircon::sys::zx_ticks_t;

/// trace_marker, used by applications to write trace events
struct TraceMarkerFileSource;

impl DynamicFileSource for TraceMarkerFileSource {
    fn generate(&self, _sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        Ok(())
    }
}

pub struct TraceMarkerFile {
    source: DynamicFile<TraceMarkerFileSource>,
    event_stacks: Mutex<HashMap<u64, Vec<(String, zx_ticks_t)>>>,
}

impl TraceMarkerFile {
    pub fn new_node() -> impl FsNodeOps {
        SimpleFileNode::new(move || {
            Ok(Self {
                source: DynamicFile::new(TraceMarkerFileSource {}),
                event_stacks: Mutex::new(HashMap::new()),
            })
        })
    }
}

impl FileOps for TraceMarkerFile {
    fileops_impl_delegate_read_and_seek!(self, self.source);

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if let Some(context) =
            TraceCategoryContext::acquire(fuchsia_trace::cstr!(trace_category_atrace!()))
        {
            let bytes = data.read_all()?;
            if let Ok(mut event_stacks) = self.event_stacks.lock() {
                let now = zx::ticks_get();
                if let Some(atrace_event) = ATraceEvent::parse(&String::from_utf8_lossy(&bytes)) {
                    match atrace_event {
                        ATraceEvent::Begin { pid, name } => {
                            event_stacks
                                .entry(pid)
                                .or_insert_with(Vec::new)
                                .push((name.to_string(), now));
                        }
                        ATraceEvent::End { pid } => {
                            if let Some(stack) = event_stacks.get_mut(&pid) {
                                if let Some((name, start_time)) = stack.pop() {
                                    context.write_duration_with_inline_name(&name, start_time, &[]);
                                }
                            }
                        }
                        ATraceEvent::Instant { name } => {
                            context.write_instant_with_inline_name(&name, Scope::Process, &[]);
                        }
                        ATraceEvent::AsyncBegin { name, correlation_id } => {
                            context.write_async_begin_with_inline_name(
                                correlation_id.into(),
                                &name,
                                &[],
                            );
                        }
                        ATraceEvent::AsyncEnd { name, correlation_id } => {
                            context.write_async_end_with_inline_name(
                                correlation_id.into(),
                                &name,
                                &[],
                            );
                        }
                        ATraceEvent::Counter { name, value } => {
                            // ATrace only supplies one name in each counter record,
                            // so it appears that counters are not intended to be grouped.
                            // As such, we use the name both for the record name and
                            // the arg name.
                            let arg = ArgValue::of(name, value);
                            context.write_counter_with_inline_name(name, 0, &[arg]);
                        }
                    }
                }
            }
            Ok(bytes.len())
        } else {
            Ok(data.drain())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ATraceEvent<'a> {
    Begin { pid: u64, name: &'a str },
    End { pid: u64 },
    Instant { name: &'a str },
    AsyncBegin { name: &'a str, correlation_id: u64 },
    AsyncEnd { name: &'a str, correlation_id: u64 },
    Counter { name: &'a str, value: i64 },
}

impl<'a> ATraceEvent<'a> {
    // Arbitrary data is allowed to be written to tracefs, and we only care about identifying ATrace
    // events to forward to Fuchsia tracing. Since we would throw away any detailed parsing error, this
    // function returns an Option rather than a Result. If we did return a Result, this could be
    // put in a TryFrom impl, if desired.
    fn parse(s: &'a str) -> Option<Self> {
        let chunks: Vec<_> = s.split('|').collect();
        if chunks.len() >= 3 && chunks[0] == "B" {
            let pid = chunks[1].parse::<u64>().ok()?;
            return Some(ATraceEvent::Begin { pid, name: chunks[2] });
        } else if chunks.len() >= 2 && chunks[0] == "E" {
            let pid = chunks[1].parse::<u64>().ok()?;
            return Some(ATraceEvent::End { pid });
        } else if chunks.len() >= 3 && chunks[0] == "I" {
            return Some(ATraceEvent::Instant { name: chunks[2] });
        } else if chunks.len() >= 4 && chunks[0] == "S" {
            let correlation_id = chunks[3].parse::<u64>().ok()?;
            return Some(ATraceEvent::AsyncBegin { name: chunks[2], correlation_id });
        } else if chunks.len() >= 4 && chunks[0] == "F" {
            let correlation_id = chunks[3].parse::<u64>().ok()?;
            return Some(ATraceEvent::AsyncEnd { name: chunks[2], correlation_id });
        } else if chunks.len() >= 4 && chunks[0] == "C" {
            let value = chunks[3].parse::<i64>().ok()?;
            return Some(ATraceEvent::Counter { name: chunks[2], value });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atrace_event_parsing() {
        assert_eq!(
            ATraceEvent::parse("B|1636|slice_name"),
            Some(ATraceEvent::Begin { pid: 1636, name: "slice_name" }),
        );
        assert_eq!(ATraceEvent::parse("E|1636"), Some(ATraceEvent::End { pid: 1636 }),);
        assert_eq!(
            ATraceEvent::parse("I|1636|instant_name"),
            Some(ATraceEvent::Instant { name: "instant_name" }),
        );
        assert_eq!(
            ATraceEvent::parse("S|1636|async_name|123"),
            Some(ATraceEvent::AsyncBegin { name: "async_name", correlation_id: 123 }),
        );
        assert_eq!(
            ATraceEvent::parse("F|1636|async_name|123"),
            Some(ATraceEvent::AsyncEnd { name: "async_name", correlation_id: 123 }),
        );
        assert_eq!(
            ATraceEvent::parse("C|1636|counter_name|123"),
            Some(ATraceEvent::Counter { name: "counter_name", value: 123 }),
        );
    }
}
