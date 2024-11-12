// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::event::{TraceEvent, TraceEventQueue};
use fuchsia_trace::{ArgValue, Scope, TraceCategoryContext};
use starnix_core::task::CurrentTask;
use starnix_core::vfs::buffers::InputBuffer;
use starnix_core::vfs::{
    fileops_impl_delegate_read_and_seek, fileops_impl_noop_sync, DynamicFile, DynamicFileBuf,
    DynamicFileSource, FileObject, FileOps, FsNodeOps, SimpleFileNode,
};
use starnix_logging::CATEGORY_ATRACE;
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errors::Errno;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// trace_marker, used by applications to write trace events
struct TraceMarkerFileSource;

impl DynamicFileSource for TraceMarkerFileSource {
    fn generate(&self, _sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        Ok(())
    }
}

pub struct TraceMarkerFile {
    source: DynamicFile<TraceMarkerFileSource>,
    event_stacks: Mutex<HashMap<u64, Vec<(String, zx::BootTicks)>>>,
    queue: Arc<TraceEventQueue>,
}

impl TraceMarkerFile {
    pub fn new_node(queue: Arc<TraceEventQueue>) -> impl FsNodeOps {
        SimpleFileNode::new(move || {
            Ok(Self {
                source: DynamicFile::new(TraceMarkerFileSource {}),
                event_stacks: Mutex::new(HashMap::new()),
                queue: queue.clone(),
            })
        })
    }
}

impl FileOps for TraceMarkerFile {
    fileops_impl_delegate_read_and_seek!(self, self.source);
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let bytes = data.read_all()?;
        if let Some(atrace_event) = ATraceEvent::parse(&String::from_utf8_lossy(&bytes)) {
            if self.queue.is_enabled() {
                let timestamp = zx::BootInstant::get();
                let trace_event = TraceEvent::new(
                    self.queue.prev_timestamp(),
                    timestamp,
                    current_task.get_pid(),
                    &bytes,
                );
                self.queue.push_event(trace_event, timestamp)?;
            }
            // TODO(https://fxbug.dev/357665908): Remove forwarding of atrace events to trace
            // manager when dependencies have been migrated.
            if let Some(context) = TraceCategoryContext::acquire(CATEGORY_ATRACE) {
                if let Ok(mut event_stacks) = self.event_stacks.lock() {
                    let now = zx::BootTicks::get();
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
        } else {
            // Ideally clearing should only be done once when we see tracing
            // stop, but the trace observer thread is behind the perfetto_consumer
            // feature, and with the way that filesystem creation is arranged it
            // is difficult to route a reference to the stacks to the thread.
            // In lieu of having a trace observer, we clear the stacks whenever
            // an event is written and starnix:atrace category is disabled.
            //
            // Other than extreme circumstances where no trace events are written
            // between the two trace sessions, this will prevent any events that
            // were started in the first trace session from being matched with
            // an end event in the second session. We don't expect such a
            // situation to occur, as it would mean that we are telling applications
            // to stop emitting atrace data before stopping the trace, where
            // a typical use case would want atrace data for the entire duration
            // of the trace.
            if let Ok(mut event_stacks) = self.event_stacks.lock() {
                event_stacks.clear();
            }
        }
        return Ok(bytes.len());
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
        let mut chunks = s.split('|');
        let event_type = chunks.next()?;
        match event_type {
            "B" => {
                let pid = chunks.next()?.parse::<u64>().ok()?;
                let name = chunks.next()?;
                Some(ATraceEvent::Begin { pid, name })
            }
            "E" => {
                let pid = chunks.next()?.parse::<u64>().ok()?;
                Some(ATraceEvent::End { pid })
            }
            "I" => {
                let _pid = chunks.next()?;
                let name = chunks.next()?;
                Some(ATraceEvent::Instant { name })
            }
            "S" => {
                let _pid = chunks.next()?;
                let name = chunks.next()?;
                let correlation_id = chunks.next()?.parse::<u64>().ok()?;
                Some(ATraceEvent::AsyncBegin { name, correlation_id })
            }
            "F" => {
                let _pid = chunks.next()?;
                let name = chunks.next()?;
                let correlation_id = chunks.next()?.parse::<u64>().ok()?;
                Some(ATraceEvent::AsyncEnd { name, correlation_id })
            }
            "C" => {
                let _pid = chunks.next()?;
                let name = chunks.next()?;
                let value = chunks.next()?.parse::<i64>().ok()?;
                Some(ATraceEvent::Counter { name, value })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
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
