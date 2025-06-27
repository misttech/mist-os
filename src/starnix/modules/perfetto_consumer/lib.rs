// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::bail;
use fuchsia_trace::{
    category_enabled, trace_string_ref_t, BufferingMode, ProlongedContext, TraceState,
};
use fxt::blob::{BlobHeader, BlobType};
use perfetto_protos::perfetto::protos::trace_config::buffer_config::FillPolicy;
use perfetto_protos::perfetto::protos::trace_config::{BufferConfig, DataSource};
use perfetto_protos::perfetto::protos::{
    ipc_frame, DataSourceConfig, DisableTracingRequest, EnableTracingRequest, FreeBuffersRequest,
    FtraceConfig, ReadBuffersRequest, ReadBuffersResponse, TraceConfig,
};
use prost::Message;
use starnix_core::task::{CurrentTask, Kernel};
use starnix_core::vfs::FsString;
use starnix_logging::{log_error, CATEGORY_ATRACE, NAME_PERFETTO_BLOB};
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::errors::Errno;

use fuchsia_async as fasync;
use fuchsia_trace_observer::TraceObserver;

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

                                        // Ignore a failure to write the packet here. We don't
                                        // return immediately because we want to allow the
                                        // remaining records to be recorded as dropped.
                                        let _ = self.forward_packet(blob_name_ref, blob_data);
                                    }
                                }
                            }
                            if reply.has_more != Some(true) {
                                break;
                            }
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
            };
            while let Ok(state) = observer.on_state_changed().await {
                callback_state.on_state_change(locked, state, &current_task).unwrap_or_else(|e| {
                    log_error!("perfetto_consumer callback error: {:?}", e);
                })
            }
        });
    });

    Ok(())
}
