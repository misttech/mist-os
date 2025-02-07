// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crossbeam_channel::Sender;
use perfetto_consumer_proto::perfetto::protos::trace_config::buffer_config::FillPolicy;
use perfetto_consumer_proto::perfetto::protos::trace_config::{BufferConfig, DataSource};
use perfetto_consumer_proto::perfetto::protos::{
    ipc_frame, DataSourceConfig, DisableTracingRequest, EnableTracingRequest, FreeBuffersRequest,
    FtraceConfig, ReadBuffersRequest, ReadBuffersResponse, TraceConfig,
};
use prost::Message;
use starnix_core::task::{CurrentTask, Kernel};
use starnix_core::vfs::FsString;
use starnix_logging::{log_debug, log_error, CATEGORY_ATRACE, NAME_PERFETTO_BLOB};
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use std::sync::{Arc, OnceLock};

use fuchsia_trace::{category_enabled, trace_state, ProlongedContext, TraceState};

/// Sender for the trace state, which sends a message each time trace state is updated.
static TRACE_STATE_SENDER: OnceLock<Sender<TraceState>> = OnceLock::new();

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
        locked: &mut Locked<'_, Unlocked>,
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
        locked: &mut Locked<'_, Unlocked>,
        new_state: TraceState,
        current_task: &CurrentTask,
    ) -> Result<(), anyhow::Error> {
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
                if self.prev_state == TraceState::Started {
                    let context = fuchsia_trace::Context::acquire();
                    // Now that we have acquired a context (or at least attempted to),
                    // we can drop the prolonged context. We want to do this early to
                    // avoid making the trace session hang if this function exits
                    // on an error path.
                    self.prolonged_context = None;

                    let disable_request;
                    let read_buffers_request;
                    let blob_name_ref;
                    {
                        let connection = self.connection(locked, current_task)?;
                        disable_request = connection.disable_tracing(
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

                        read_buffers_request =
                            connection.read_buffers(locked, current_task, ReadBuffersRequest {})?;
                        blob_name_ref = context
                            .as_ref()
                            .map(|context| context.register_string_literal(NAME_PERFETTO_BLOB));
                    }

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
                                        if let Some(context) = &context {
                                            context.write_blob_record(
                                                fuchsia_trace::TRACE_BLOB_TYPE_PERFETTO,
                                                blob_name_ref.as_ref().expect(
                                                    "blob_name_ref is Some whenever context is",
                                                ),
                                                blob_data.as_slice(),
                                            );
                                        }
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
                }
            }
        }
        self.prev_state = new_state;
        Ok(())
    }
}

pub fn start_perfetto_consumer_thread(
    kernel: &Arc<Kernel>,
    socket_path: FsString,
) -> Result<(), Errno> {
    let (sender, receiver) = crossbeam_channel::unbounded::<TraceState>();
    kernel.kthreads.spawner().spawn({
        move |locked, current_task| {
            let mut callback_state = CallbackState {
                prev_state: TraceState::Stopped,
                socket_path,
                connection: None,
                prolonged_context: None,
                packet_data: Vec::new(),
            };
            let receiver = current_task.kernel().on_shutdown.wrap_channel(receiver);
            loop {
                match receiver.recv() {
                    Some(Ok(state)) => callback_state
                        .on_state_change(locked, state, &current_task)
                        .unwrap_or_else(|e| {
                            log_error!("perfetto_consumer callback error: {:?}", e);
                        }),
                    Some(Err(e)) => {
                        log_error!(e:?; "exiting perfetto consumer thread due to error");
                        break;
                    }
                    None => {
                        log_debug!("exiting perfetto consumer thread");
                        break;
                    }
                }
            }
        }
    });
    // Store the other end of the channel so that it can be called from a static callback function
    // when the trace is changed.
    TRACE_STATE_SENDER.set(sender).or_else(|e| {
        log_error!("Failed to set perfetto_consumer trace state sender: {:?}", e);
        Err(errno!(EINVAL))
    })?;
    fuchsia_trace_observer::start_trace_observer(c_callback);
    Ok(())
}

extern "C" fn c_callback() {
    let state = trace_state();
    if let Some(sender) = TRACE_STATE_SENDER.get() {
        sender.send(state).unwrap_or_else(|e| log_error!("perfetto_consumer send failed: {:?}", e));
    } else {
        log_error!("perfetto_consumer sender was not set when the callback was called");
    }
}
