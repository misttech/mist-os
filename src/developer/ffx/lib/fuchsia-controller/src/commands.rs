// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::env_context::{EnvContext, FfxConfigEntry};
use crate::ext_buffer::ExtBuffer;
use crate::lib_context::LibContext;
use crate::waker::handle_notifier_waker;
use fidl::{AsHandleRef, HandleBased};
use fuchsia_zircon_status as zx_status;
use fuchsia_zircon_types as zx_types;
use std::mem::ManuallyDrop;
use std::mem::MaybeUninit;
use std::sync::{mpsc, Arc};
use std::task::{Context, Poll};

type Responder<T> = mpsc::SyncSender<T>;
type CmdResult<T> = Result<T, zx_status::Status>;

pub(crate) struct ReadResponse {
    pub(crate) actual_bytes_count: usize,
    pub(crate) actual_handles_count: usize,
    pub(crate) result: zx_status::Status,
}

pub(crate) enum LibraryCommand {
    ShutdownLib,
    GetNotificationDescriptor {
        lib: Arc<LibContext>,
        responder: Responder<i32>,
    },
    CreateEnvContext {
        lib: Arc<LibContext>,
        responder: Responder<CmdResult<Arc<EnvContext>>>,
        config: Vec<FfxConfigEntry>,
    },
    OpenDaemonProtocol {
        env: Arc<EnvContext>,
        protocol: String,
        responder: Responder<CmdResult<zx_types::zx_handle_t>>,
    },
    OpenDeviceProxy {
        env: Arc<EnvContext>,
        moniker: String,
        capability_name: String,
        responder: Responder<CmdResult<zx_types::zx_handle_t>>,
    },
    OpenTargetProxy {
        env: Arc<EnvContext>,
        responder: Responder<CmdResult<zx_types::zx_handle_t>>,
    },
    OpenRemoteControlProxy {
        env: Arc<EnvContext>,
        responder: Responder<CmdResult<zx_types::zx_handle_t>>,
    },
    ChannelRead {
        lib: Arc<LibContext>,
        channel: fidl::Channel,
        out_buf: ExtBuffer<u8>,
        out_handles: ExtBuffer<MaybeUninit<fidl::Handle>>,
        responder: Responder<ReadResponse>,
    },
    ChannelWrite {
        channel: fidl::Channel,
        buf: ExtBuffer<u8>,
        handles: ExtBuffer<fidl::Handle>,
        responder: Responder<zx_status::Status>,
    },
    SocketRead {
        lib: Arc<LibContext>,
        socket: fidl::Socket,
        out_buf: ExtBuffer<u8>,
        responder: Responder<ReadResponse>,
    },
    SocketWrite {
        socket: fidl::Socket,
        buf: ExtBuffer<u8>,
        responder: Responder<zx_status::Status>,
    },
}

impl LibraryCommand {
    pub(crate) async fn run(self) {
        match self {
            Self::ShutdownLib => panic!("unsupported command. exiting thread."),
            Self::GetNotificationDescriptor { lib, responder } => {
                match lib.notifier_descriptor().await {
                    Ok(r) => {
                        responder.send(r).unwrap();
                    }
                    Err(e) => {
                        lib.write_err(e);
                        responder.send(zx_status::Status::INTERNAL.into_raw()).unwrap();
                    }
                }
            }
            Self::CreateEnvContext { lib, config, responder } => {
                match EnvContext::new(Arc::downgrade(&lib), config).await {
                    Ok(e) => {
                        responder.send(Ok(Arc::new(e))).unwrap();
                    }
                    Err(e) => {
                        lib.write_err(e);
                        responder.send(Err(zx_status::Status::INTERNAL)).unwrap();
                    }
                }
            }
            Self::OpenDaemonProtocol { env, protocol, responder } => {
                match env.connect_daemon_protocol(protocol).await {
                    Ok(r) => {
                        responder.send(Ok(r)).unwrap();
                    }
                    Err(e) => {
                        env.write_err(e);
                        responder.send(Err(zx_status::Status::INTERNAL)).unwrap();
                    }
                }
            }
            Self::OpenTargetProxy { env, responder } => match env.connect_target_proxy().await {
                Ok(h) => {
                    responder.send(Ok(h)).unwrap();
                }
                Err(e) => {
                    env.write_err(e);
                    responder.send(Err(zx_status::Status::INTERNAL)).unwrap();
                }
            },
            Self::OpenRemoteControlProxy { env, responder } => {
                match env.connect_remote_control_proxy().await {
                    Ok(h) => {
                        responder.send(Ok(h)).unwrap();
                    }
                    Err(e) => {
                        env.write_err(e);
                        responder.send(Err(zx_status::Status::INTERNAL)).unwrap();
                    }
                }
            }
            Self::OpenDeviceProxy { env, moniker, capability_name, responder } => {
                match env.connect_device_proxy(moniker, capability_name).await {
                    Ok(r) => {
                        responder.send(Ok(r)).unwrap();
                    }
                    Err(e) => {
                        env.write_err(e);
                        responder.send(Err(zx_status::Status::INTERNAL)).unwrap();
                    }
                }
            }
            Self::ChannelRead { lib, channel, mut out_buf, mut out_handles, responder } => {
                let channel = match fidl::AsyncChannel::from_channel(channel) {
                    Ok(c) => c,
                    Err(e) => {
                        lib.write_err(e);
                        responder
                            .send(ReadResponse {
                                actual_bytes_count: 0,
                                actual_handles_count: 0,
                                result: zx_status::Status::INTERNAL,
                            })
                            .unwrap();
                        return;
                    }
                };
                // Creates a waker that can notify when the channel needs to be
                // woken for reads. Does not actually cause any reads to happen. One
                // must manually invoke reading from this channel in a subsequent
                // call.
                let waker =
                    handle_notifier_waker(channel.raw_handle(), lib.notification_sender().await);
                let task_ctx = &mut Context::from_waker(&waker);
                let res = match channel.read_raw(task_ctx, &mut out_buf, &mut out_handles) {
                    Poll::Ready(res) => res,
                    Poll::Pending => {
                        // Don't want to drop the channel since it is pending.
                        let _channel = ManuallyDrop::new(channel.into_zx_channel());
                        responder
                            .send(ReadResponse {
                                actual_bytes_count: 0,
                                actual_handles_count: 0,
                                result: zx_status::Status::SHOULD_WAIT,
                            })
                            .unwrap();
                        return;
                    }
                };
                let _channel = ManuallyDrop::new(channel.into_zx_channel());
                match res {
                    Ok((res, actual_bytes_count, actual_handles_count)) => match res {
                        Err(e) => {
                            responder
                                .send(ReadResponse {
                                    actual_bytes_count,
                                    actual_handles_count,
                                    result: e,
                                })
                                .unwrap();
                        }
                        Ok(()) => {
                            responder
                                .send(ReadResponse {
                                    actual_bytes_count,
                                    actual_handles_count,
                                    result: zx_status::Status::OK,
                                })
                                .unwrap();
                        }
                    },
                    Err((actual_bytes_count, actual_handles_count)) => {
                        responder
                            .send(ReadResponse {
                                actual_bytes_count,
                                actual_handles_count,
                                result: zx_status::Status::BUFFER_TOO_SMALL,
                            })
                            .unwrap();
                    }
                }
            }
            Self::ChannelWrite { channel, buf, mut handles, responder } => {
                let channel = ManuallyDrop::new(channel);
                let status = match channel.write(&buf, &mut handles) {
                    Ok(_) => zx_status::Status::OK,
                    Err(e) => e,
                };
                responder.send(status).unwrap();
            }
            Self::SocketRead { lib, socket, mut out_buf, responder } => {
                let socket = match fidl::AsyncSocket::from_socket(socket) {
                    Ok(s) => s,
                    Err(e) => {
                        lib.write_err(e);
                        responder
                            .send(ReadResponse {
                                actual_bytes_count: 0,
                                actual_handles_count: 0,
                                result: zx_status::Status::INTERNAL,
                            })
                            .unwrap();
                        return;
                    }
                };
                let waker =
                    handle_notifier_waker(socket.raw_handle(), lib.notification_sender().await);
                let task_ctx = &mut Context::from_waker(&waker);
                let res = match socket.poll_read_ref(task_ctx, &mut out_buf) {
                    Poll::Ready(res) => res,
                    Poll::Pending => {
                        let _ = socket.into_zx_socket().into_raw();
                        responder
                            .send(ReadResponse {
                                actual_bytes_count: 0,
                                actual_handles_count: 0,
                                result: zx_status::Status::SHOULD_WAIT,
                            })
                            .unwrap();
                        return;
                    }
                };
                let _ = socket.into_zx_socket().into_raw();
                match res {
                    Err(e) => responder
                        .send(ReadResponse {
                            actual_bytes_count: 0,
                            actual_handles_count: 0,
                            result: e,
                        })
                        .unwrap(),
                    Ok(size) => responder
                        .send(ReadResponse {
                            actual_handles_count: 0,
                            actual_bytes_count: size,
                            result: zx_status::Status::OK,
                        })
                        .unwrap(),
                }
            }
            Self::SocketWrite { socket, buf, responder } => {
                let socket = ManuallyDrop::new(socket);
                let status = match socket.write(&buf) {
                    Ok(_) => zx_status::Status::OK,
                    Err(e) => e,
                };
                responder.send(status).unwrap();
            }
        }
    }
}
