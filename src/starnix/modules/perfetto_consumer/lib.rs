// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::bail;
use fuchsia_async as fasync;
use fuchsia_trace::{
    category_enabled, trace_string_ref_t, BufferingMode, ProlongedContext, TraceState,
};
use fuchsia_trace_observer::TraceObserver;
use fxt::blob::{BlobHeader, BlobType};
use perfetto_protos::perfetto::protos::trace_config::buffer_config::FillPolicy;
use perfetto_protos::perfetto::protos::trace_config::{BufferConfig, DataSource};
use perfetto_protos::perfetto::protos::{
    ipc_frame, DataSourceConfig, DisableTracingRequest, EnableTracingRequest, FreeBuffersRequest,
    FtraceConfig, ReadBuffersRequest, ReadBuffersResponse, TraceConfig,
};
use perfetto_trace_protos::perfetto::protos::frame_timeline_event::{
    ActualDisplayFrameStart, ActualSurfaceFrameStart, Event, ExpectedDisplayFrameStart,
    ExpectedSurfaceFrameStart,
};
use perfetto_trace_protos::perfetto::protos::ftrace_event::Event::Print;
use perfetto_trace_protos::perfetto::protos::{trace_packet, Trace};
use prost::Message;
use starnix_core::task::{CurrentTask, Kernel};
use starnix_core::vfs::FsString;
use starnix_logging::{log_error, log_info, CATEGORY_ATRACE, NAME_PERFETTO_BLOB};
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::errors::Errno;
use starnix_uapi::pid_t;
use std::collections::HashMap;
use std::sync::Arc;
use zx::Koid;

mod atrace;

const PERFETTO_BUFFER_SIZE_KB: u32 = 63488;

/// State needed to act upon trace state changes.
struct CallbackState {
    /// The previously observed trace state.
    prev_state: TraceState,
    /// Path to the Perfetto consumer socket.
    socket_path: FsString,
    /// Connection to the consumer socket, if it has been initialized. This gets initialized the
    /// first time it is needed.
    connection: Option<perfetto::Consumer>,
    /// Prolonged trace context to prevent the Fuchsia trace session from terminating while reading
    /// data from Perfetto.
    prolonged_context: Option<ProlongedContext>,
    /// Partial trace packet returned from Perfetto but not yet written to Fuchsia.
    packet_data: Vec<u8>,

    /// A mapping of Linux pids to Fuchsia koid. This is used
    /// to patch up the perfetto data so it aligns with the Fuchsia processes
    /// in the trace.
    pid_map: HashMap<pid_t, Koid>,
}

impl CallbackState {
    fn connection(
        &mut self,
        locked: &mut Locked<Unlocked>,
        current_task: &CurrentTask,
    ) -> Result<&mut perfetto::Consumer, anyhow::Error> {
        match self.connection {
            None => {
                self.connection =
                    Some(perfetto::Consumer::new(locked, current_task, self.socket_path.as_ref())?);
                Ok(self.connection.as_mut().unwrap())
            }
            Some(ref mut conn) => Ok(conn),
        }
    }

    fn on_state_change(
        &mut self,
        locked: &mut Locked<Unlocked>,
        new_state: TraceState,
        current_task: &CurrentTask,
    ) -> Result<(), anyhow::Error> {
        let prev_state = self.prev_state;
        self.prev_state = new_state;
        match new_state {
            TraceState::Started => {
                self.prolonged_context = ProlongedContext::acquire();
                let connection = self.connection(locked, current_task)?;
                // A fixed set of data sources that may be of interest. As demand for other sources
                // is found, add them here, and it may become worthwhile to allow this set to be
                // configurable per trace session.
                let mut data_sources = vec![
                    DataSource {
                        config: Some(DataSourceConfig {
                            name: Some("track_event".to_string()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    DataSource {
                        config: Some(DataSourceConfig {
                            name: Some("android.surfaceflinger.frame".to_string()),
                            target_buffer: Some(0),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    DataSource {
                        config: Some(DataSourceConfig {
                            name: Some("android.surfaceflinger.frametimeline".to_string()),
                            target_buffer: Some(0),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ];
                if category_enabled(CATEGORY_ATRACE) {
                    data_sources.push(DataSource {
                        config: Some(DataSourceConfig {
                            name: Some("linux.ftrace".to_string()),
                            ftrace_config: Some(FtraceConfig {
                                ftrace_events: vec!["ftrace/print".to_string()],
                                // Enable all supported atrace categories. This could be improved
                                // in the future to be a subset that is configurable by each trace
                                // session.
                                atrace_categories: vec![
                                    "am".to_string(),
                                    "adb".to_string(),
                                    "aidl".to_string(),
                                    "dalvik".to_string(),
                                    "audio".to_string(),
                                    "binder_lock".to_string(),
                                    "binder_driver".to_string(),
                                    "bionic".to_string(),
                                    "camera".to_string(),
                                    "database".to_string(),
                                    "gfx".to_string(),
                                    "hal".to_string(),
                                    "input".to_string(),
                                    "network".to_string(),
                                    "nnapi".to_string(),
                                    "pm".to_string(),
                                    "power".to_string(),
                                    "rs".to_string(),
                                    "res".to_string(),
                                    "rro".to_string(),
                                    "sched".to_string(),
                                    "sm".to_string(),
                                    "ss".to_string(),
                                    "vibrator".to_string(),
                                    "video".to_string(),
                                    "view".to_string(),
                                    "webview".to_string(),
                                    "wm".to_string(),
                                ],
                                atrace_apps: vec!["*".to_string()],
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
                }
                connection.enable_tracing(
                    locked,
                    current_task,
                    EnableTracingRequest {
                        trace_config: Some(TraceConfig {
                            buffers: vec![BufferConfig {
                                size_kb: Some(PERFETTO_BUFFER_SIZE_KB),
                                fill_policy: Some(FillPolicy::Discard.into()),
                                ..Default::default()
                            }],
                            data_sources,
                            ..Default::default()
                        }),
                        attach_notification_only: None,
                    },
                )?;
            }
            TraceState::Stopping | TraceState::Stopped => {
                if prev_state == TraceState::Started {
                    // We want to hold the prolonged context to ensure the trace session doesn't
                    // exit out from under us, but we also want to ensure we drop the prolonged
                    // context if we bail for whatever reason below.
                    let _local_prolonged_context =
                        std::mem::replace(&mut self.prolonged_context, None);

                    let connection = self.connection(locked, current_task)?;
                    let disable_request = connection.disable_tracing(
                        locked,
                        current_task,
                        DisableTracingRequest {},
                    )?;
                    loop {
                        let frame = connection.next_frame_blocking(locked, current_task)?;
                        if frame.request_id == Some(disable_request) {
                            break;
                        } else {
                            log_error!(
                                "Ignoring frame while looking for DisableTracingRequest: {frame:?}"
                            );
                        }
                    }

                    let read_buffers_request =
                        connection.read_buffers(locked, current_task, ReadBuffersRequest {})?;

                    let blob_name_ref = {
                        let Some(context) = fuchsia_trace::Context::acquire() else {
                            bail!("Tracing stopped despite holding prolonged context");
                        };
                        context.register_string_literal(NAME_PERFETTO_BLOB)
                    };

                    // IPC responses may be spread across multiple frames, so loop until we get a
                    // message that indicates it is the last one. Additionally, if there are
                    // unrelated messages on the socket (e.g. leftover from a previous trace
                    // session), the loop will read past and ignore them.
                    loop {
                        let frame = self
                            .connection(locked, current_task)?
                            .next_frame_blocking(locked, current_task)?;
                        if frame.request_id != Some(read_buffers_request) {
                            continue;
                        } else {
                            log_info!(
                                "perfetto_consumer ignoring frame while looking for ReadBuffersRequest {read_buffers_request}: {frame:?}"
                            );
                        }
                        if let Some(ipc_frame::Msg::MsgInvokeMethodReply(reply)) = &frame.msg {
                            if let Ok(response) = ReadBuffersResponse::decode(
                                reply.reply_proto.as_deref().unwrap_or(&[]),
                            ) {
                                for slice in &response.slices {
                                    if let Some(data) = &slice.data {
                                        self.packet_data.extend(data);
                                    }
                                    if slice.last_slice_for_packet.unwrap_or(false) {
                                        let mut blob_data = Vec::new();
                                        // Packet field number = 1, length delimited type = 2.
                                        blob_data.push(1 << 3 | 2);
                                        // Push a varint encoded length.
                                        // See https://protobuf.dev/programming-guides/encoding/
                                        const HIGH_BIT: u8 = 0x80;
                                        const LOW_SEVEN_BITS: usize = 0x7F;
                                        let mut value = self.packet_data.len();
                                        while value >= HIGH_BIT as usize {
                                            blob_data
                                                .push((value & LOW_SEVEN_BITS) as u8 | HIGH_BIT);
                                            value >>= 7;
                                        }
                                        blob_data.push(value as u8);
                                        // `append` moves all data out of the passed Vec, so
                                        // s.packet_data will be empty after this call.
                                        blob_data.append(&mut self.packet_data);

                                        // At this point blob_data is a full Perfetto Trace protobuf.
                                        // Parse the data and replace the linux pids with their
                                        // corresponding koid.
                                        let rewritten =
                                            self.rewrite_pids(&blob_data).unwrap_or(blob_data);

                                        // Ignore a failure to write the packet here. We don't
                                        // return immediately because we want to allow the
                                        // remaining records to be recorded as dropped.
                                        if self.forward_packet(blob_name_ref, rewritten).is_none() {
                                            log_error!("perfetto_consumer packet was not forwarded successfully");
                                        }
                                    }
                                }
                            } else {
                                log_error!(
                                    "perfetto_consumer cannot decode protobuf from {reply:?}"
                                );
                            }
                            if reply.has_more != Some(true) {
                                break;
                            }
                        } else {
                            log_error!("perfetto_consumer ignoring non-MsgInvokeMethodReply message: {frame:?}");
                        }
                    }
                    // The response to a free buffers request does not have anything meaningful,
                    // so we don't need to worry about tracking the request id to match to the
                    // response.
                    let _free_buffers_request_id =
                        self.connection(locked, current_task)?.free_buffers(
                            locked,
                            current_task,
                            FreeBuffersRequest { buffer_ids: vec![0] },
                        )?;
                } else {
                    // If we receive a stop request and we don't think we're actually tracing, our
                    // local state likely desynced from the global trace state. Clean up our state
                    // and ensure we're stopped so we re-synchronize.
                    self.prolonged_context = None;
                    self.packet_data.clear();
                    self.pid_map.clear();
                }
            }
        }
        Ok(())
    }

    // Forward `data` to the trace buffer by wrapping it in fxt blob records with the name
    // `blob_name_ref`..
    fn forward_packet(&self, blob_name_ref: trace_string_ref_t, data: Vec<u8>) -> Option<usize> {
        // The blob data may be larger than what we can fit in a single record. If so, split it up
        // over multiple chunks.
        let mut bytes_written = 0;
        let mut data_to_write = &data[..];

        // We want to break the data into chunks:
        // - Bigger chunks means less per-write overheader
        // - Bigger chunks means less overhead due to blob meta
        //
        // However, too big and the blobs won't fit nicely into the trace buffer.
        // The trace buffer is minimum 1MiB in size, so writing 4k at a time seems like a
        // reasonable place to start that is both reasonably large and not going to leave a ton of
        // space at the end of the trace buffer.
        let max_chunk_size = 4096;
        while !data_to_write.is_empty() {
            let chunk_size = data_to_write.len().min(max_chunk_size);
            let chunk = &data_to_write[..chunk_size];
            self.forward_blob(blob_name_ref, &chunk)?;
            data_to_write = &data_to_write[chunk_size..];
            bytes_written += chunk_size;
        }
        Some(bytes_written)
    }

    // Given a blob name, wrap the data in an fxt perfetto blob and write it to the trace buffer.
    fn forward_blob(&self, blob_name_ref: trace_string_ref_t, blob_data: &[u8]) -> Option<usize> {
        let mut header = BlobHeader::empty();
        header.set_name_ref(blob_name_ref.encoded_value);
        header.set_payload_len(blob_data.len() as u16);
        header.set_blob_format_type(BlobType::Perfetto.into());

        let record_bytes = fxt::fxt_builder::FxtBuilder::new(header).atom(blob_data).build();
        assert!(record_bytes.len() % std::mem::size_of::<u64>() == 0);
        let num_words = record_bytes.len() / std::mem::size_of::<u64>();
        let record_data = record_bytes.as_ptr();
        let record_words =
            unsafe { std::slice::from_raw_parts(record_data.cast::<u64>(), num_words) };

        while let Some(context) = fuchsia_trace::Context::acquire() {
            if let Some(bytes) = context.copy_record(record_words) {
                return Some(bytes);
            }
            if context.buffering_mode() != BufferingMode::Streaming {
                // If we're not in streaming mode, there will never be room for this record. Drop
                // it.
                return None;
            }
            // We're writing records pretty quick here, we're just forwarding data from
            // perfetto with no breaks. trace_manager might not be able to keep up if it's also
            // servicing other trace-providers. We want to back off we if find that we run out
            // of space.
            //
            // We drop the context to decrement the refcount on the trace session. This allows
            // trace-engine to switch the buffers if needed and drain out the buffers so that
            // when we wake, there will hopefully be room.
            //
            // TODO(b/304532640)
            drop(context);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        None
    }

    fn rewrite_pids(&self, protobuf_blob: &Vec<u8>) -> anyhow::Result<Vec<u8>> {
        let mut proto = Trace::decode(protobuf_blob.as_slice())?;
        for ref mut p in &mut proto.packet {
            if let Some(ref mut data) = p.data {
                match data {
                    trace_packet::Data::FrameTimelineEvent(ref mut frame_timeline_event) => {
                        if let Some(ref mut evt) = &mut frame_timeline_event.event {
                            // Update the linux pid to the Fuchsia pid. Each event has its own
                            // match arm since the variant data is of a different type for each event.
                            match evt {
                                Event::ExpectedDisplayFrameStart(ExpectedDisplayFrameStart {
                                    ref mut pid,
                                    ..
                                })
                                | Event::ActualDisplayFrameStart(ActualDisplayFrameStart {
                                    ref mut pid,
                                    ..
                                })
                                | Event::ExpectedSurfaceFrameStart(ExpectedSurfaceFrameStart {
                                    ref mut pid,
                                    ..
                                })
                                | Event::ActualSurfaceFrameStart(ActualSurfaceFrameStart {
                                    ref mut pid,
                                    ..
                                }) => {
                                    pid.as_mut().map(|pid| {
                                        *pid = self.map_to_koid_val(*pid);
                                    });
                                }
                                Event::FrameEnd(_frame_end) => {}
                            }
                        }
                    }
                    trace_packet::Data::FtraceEvents(ref mut ftrace_bundle) => {
                        for ref mut evt in &mut ftrace_bundle.event {
                            if let Some(ref mut pid) = evt.pid {
                                *pid = self.map_to_koid_val(*pid as i32) as u32;
                            }
                            if let Some(ref mut event_data) = evt.event {
                                match event_data {
                                    Print(ref mut print) => {
                                        if let Some(ref mut data) = print.buf {
                                            *data = self.map_print_event(data)
                                        }
                                    }
                                    _ => (),
                                }
                            }
                        }
                    }
                    // No need to process other data; we only fixup data that references the pid.
                    _ => (),
                }
            }
        }
        Ok(proto.encode_to_vec())
    }

    fn map_print_event(&self, data: &String) -> String {
        if let Some(mut event) = atrace::ATraceEvent::parse(&data) {
            match event {
                atrace::ATraceEvent::Begin { ref mut pid, .. }
                | atrace::ATraceEvent::End { ref mut pid }
                | atrace::ATraceEvent::Instant { ref mut pid, .. }
                | atrace::ATraceEvent::AsyncBegin { ref mut pid, .. }
                | atrace::ATraceEvent::AsyncEnd { ref mut pid, .. }
                | atrace::ATraceEvent::Counter { ref mut pid, .. }
                | atrace::ATraceEvent::AsyncTrackBegin { ref mut pid, .. }
                | atrace::ATraceEvent::AsyncTrackEnd { ref mut pid, .. }
                | atrace::ATraceEvent::Track { ref mut pid, .. } => {
                    *pid = self.map_to_koid_val(*pid as i32) as u64
                }
            }
            event.data()
        } else {
            data.to_string()
        }
    }

    fn map_to_koid_val(&self, pid: i32) -> i32 {
        // Truncate the koid down to 32 bits in order to match the perfetto data schema. This is
        // usually not an issue except for artificial koids which have the 2^63 bit set, such as
        // virtual threads.This is consistent with the perfetto data importer code:
        // https://github.com/google/perfetto/blob/main/src/trace_processor/importers/fuchsia/fuchsia_trace_tokenizer.cc#L488
        self.pid_map.get(&pid).map(|k| k.raw_koid() as i32).unwrap_or(pid)
    }

    // Use the kernel pid table to make a mapping from linux pid to koid.
    fn generate_pid_mapping(&mut self, kernel: Arc<Kernel>) {
        let pid_table = kernel.pids.read();

        // The map is cleared when the tracing state is STOPPED. Only generate the map once per
        // session.
        if !self.pid_map.is_empty() {
            log_info!("perfetto_consumer already generated {} pid mappings", self.pid_map.len());
            return;
        }

        let ids = pid_table.process_ids();
        for pid in &ids {
            if let Some(tg) = pid_table.get_thread_group(*pid) {
                if let Ok(koid) = tg.get_process_koid() {
                    self.pid_map.insert(*pid, koid);
                }
            }
        }
        log_info!("perfetto_consumer recorded {} pid mappings", ids.len());
    }
}

pub fn start_perfetto_consumer_thread(kernel: &Kernel, socket_path: FsString) -> Result<(), Errno> {
    // We unfortunately need to spawn a dedicated thread to run our async task.
    //
    // While the TraceObserver waits asynchronously, the interactions we do with Perfetto over the
    // vfs::socket are blocking.
    //
    // It blocks in two scenarios:
    // 1) When we forward a control plane request over the socket and block for a response. This is
    //    for a few ms. See `perfetto::Consumer::enable_tracing`.
    // 2) When a trace ends, we repeatedly do blocking reads on the socket until we read and
    //    forward all the trace data. This servicing of trace data would hold the executor for
    //    several seconds. See `perfetto::Consumer::next_frame_blocking`.
    kernel.kthreads.spawner().spawn(|locked, current_task| {
        let mut executor = fasync::LocalExecutor::new();
        executor.run_singlethreaded(async move {
            let observer = TraceObserver::new();
            let mut callback_state = CallbackState {
                prev_state: TraceState::Stopped,
                socket_path,
                connection: None,
                prolonged_context: None,
                packet_data: Vec::new(),
                pid_map: HashMap::new(),
            };
            while let Ok(state) = observer.on_state_changed().await {
                if state == TraceState::Stopping {
                    // Generate the pid to koid mapping table before we read the perfetto data.
                    // This is a best-effort to map the linux pids to Fuchsia koids. It is possible
                    // for a linux process to emit trace data and then exit during the trace. This
                    // would result in the kernel pid table not having a mapping. This is assumed to
                    // be a rare occurrence for processes that are of interest in the trace. If it does
                    // happen, one possible fix would be to capture the pid-koid mapping when starting
                    // the trace as well.
                    callback_state.generate_pid_mapping(current_task.kernel.clone());
                }
                callback_state.on_state_change(locked, state, &current_task).unwrap_or_else(|e| {
                    log_error!("perfetto_consumer callback error: {:?}", e);
                })
            }
        });
    });

    Ok(())
}
