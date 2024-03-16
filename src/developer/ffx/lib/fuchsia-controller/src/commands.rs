// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::env_context::{EnvContext, FfxConfigEntry};
use crate::ext_buffer::ExtBuffer;
use crate::lib_context::LibContext;
use crate::waker::handle_notifier_waker;
use ffx_target::{knock_target, KnockError};
use fidl::{
    AsHandleRef, HandleBased, HandleDisposition, HandleOp, ObjectType, Peered, Rights, Status,
};
use fuchsia_async::OnSignalsRef;
use fuchsia_zircon_status as zx_status;
use fuchsia_zircon_types as zx_types;
use std::future::Future;
use std::mem::ManuallyDrop;
use std::mem::MaybeUninit;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{mpsc, Arc};
use std::task::{Context, Poll};
use std::time::Duration;
use timeout::timeout;

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
        isolate_dir: Option<PathBuf>,
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
    ChannelCreate {
        responder: Responder<(fidl::Channel, fidl::Channel)>,
    },
    ChannelWrite {
        channel: fidl::Channel,
        buf: ExtBuffer<u8>,
        handles: ExtBuffer<fidl::Handle>,
        responder: Responder<zx_status::Status>,
    },
    ConfigGetString {
        env_ctx: Arc<EnvContext>,
        config_key: String,
        out_buf: ExtBuffer<u8>,
        responder: Responder<Result<usize, zx_status::Status>>,
    },
    ChannelWriteEtc {
        channel: fidl::Channel,
        buf: ExtBuffer<u8>,
        handles: ExtBuffer<zx_types::zx_handle_disposition_t>,
        responder: Responder<zx_status::Status>,
    },
    EventCreate {
        responder: Responder<fidl::Event>,
    },
    EventPairCreate {
        responder: Responder<(fidl::EventPair, fidl::EventPair)>,
    },
    ObjectSignal {
        handle: fidl::Handle,
        clear_mask: fidl::Signals,
        set_mask: fidl::Signals,
        responder: Responder<zx_status::Status>,
    },
    ObjectSignalPeer {
        handle: fidl::Handle,
        clear_mask: fidl::Signals,
        set_mask: fidl::Signals,
        responder: Responder<zx_status::Status>,
    },
    ObjectSignalPoll {
        lib: Arc<LibContext>,
        handle: fidl::Handle,
        signals: fidl::Signals,
        responder: Responder<CmdResult<fidl::Signals>>,
    },
    SocketCreate {
        options: fidl::SocketOpts,
        responder: Responder<(fidl::Socket, fidl::Socket)>,
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
    TargetAdd {
        env: Arc<EnvContext>,
        addr: IpAddr,
        scope_id: u32,
        port: u16,
        wait: bool,
        responder: Responder<zx_status::Status>,
    },
    TargetWait {
        env: Arc<EnvContext>,
        timeout: f64,
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
            Self::CreateEnvContext { lib, config, responder, isolate_dir } => {
                match EnvContext::new(Arc::downgrade(&lib), config, isolate_dir).await {
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
                let channel = fidl::AsyncChannel::from_channel(channel);
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
            Self::ChannelCreate { responder } => {
                responder.send(fidl::Channel::create()).unwrap();
            }
            Self::ChannelWrite { channel, buf, mut handles, responder } => {
                let channel = ManuallyDrop::new(channel);
                let status = match channel.write(&buf, &mut handles) {
                    Ok(_) => zx_status::Status::OK,
                    Err(e) => e,
                };
                responder.send(status).unwrap();
            }
            Self::ChannelWriteEtc { channel, buf, mut handles, responder } => {
                let channel = ManuallyDrop::new(channel);
                let mut handle_dispositions =
                    Vec::with_capacity(zx_types::ZX_CHANNEL_MAX_MSG_HANDLES as usize);
                // The verification pass is just to eliminate some headaches around lifetime checks
                // regarding the allocation of channels from raw handle numbers.
                for i in 0..handles.len() {
                    let disp = handles[i];
                    if disp.operation != zx_types::ZX_HANDLE_OP_MOVE {
                        responder.send(zx_status::Status::NOT_SUPPORTED).unwrap();
                        return;
                    }
                    if Rights::from_bits(disp.rights).is_none() {
                        responder.send(zx_status::Status::INVALID_ARGS).unwrap();
                        return;
                    }
                }
                for i in 0..handles.len() {
                    let disp = std::mem::replace(
                        &mut handles[i],
                        zx_types::zx_handle_disposition_t {
                            operation: 0,
                            handle: 0,
                            type_: 0,
                            result: 0,
                            rights: 0,
                        },
                    );
                    let handle_op = HandleOp::Move(unsafe { fidl::Handle::from_raw(disp.handle) });
                    let object_type = ObjectType::from_raw(disp.type_);
                    let rights = Rights::from_bits(disp.rights).unwrap();
                    let result = Status::from_raw(disp.result);
                    handle_dispositions.push(HandleDisposition {
                        handle_op,
                        object_type,
                        rights,
                        result,
                    });
                }
                let status = match channel.write_etc(
                    &buf,
                    handle_dispositions.as_mut_slice() as &mut [HandleDisposition<'_>],
                ) {
                    Ok(_) => zx_status::Status::OK,
                    Err(e) => e,
                };
                responder.send(status).unwrap();
            }
            Self::ConfigGetString { env_ctx, responder, config_key, mut out_buf } => {
                let result: String = match env_ctx.context.get(&config_key).await {
                    Ok(r) => r,
                    Err(e) => {
                        env_ctx.write_err(e);
                        responder.send(Err(zx_status::Status::NOT_FOUND)).unwrap();
                        return;
                    }
                };
                let result_bytes = result.as_bytes();
                if out_buf.len() < result_bytes.len() {
                    responder.send(Err(zx_status::Status::BUFFER_TOO_SMALL)).unwrap();
                    return;
                }
                out_buf[..result_bytes.len()].copy_from_slice(result_bytes);
                responder.send(Ok(result_bytes.len())).unwrap();
            }
            Self::EventCreate { responder } => {
                responder.send(fidl::Event::create()).unwrap();
            }
            Self::EventPairCreate { responder } => {
                responder.send(fidl::EventPair::create()).unwrap();
            }
            Self::ObjectSignal { handle, clear_mask, set_mask, responder } => {
                let handle = ManuallyDrop::new(handle);
                let status = match handle.signal_handle(clear_mask, set_mask) {
                    Ok(_) => zx_status::Status::OK,
                    Err(e) => e,
                };
                responder.send(status).unwrap();
            }
            Self::ObjectSignalPeer { handle, clear_mask, set_mask, responder } => {
                // Any handle that has a peer can be converted into an EventPair.
                let handle: ManuallyDrop<fidl::EventPair> = ManuallyDrop::new(handle.into());
                let status = match handle.signal_peer(clear_mask, set_mask) {
                    Ok(_) => zx_status::Status::OK,
                    Err(e) => e,
                };
                responder.send(status).unwrap();
            }
            Self::ObjectSignalPoll { lib, handle, signals, responder } => {
                let mut on_signals = OnSignalsRef::new(&handle, signals);
                let waker =
                    handle_notifier_waker(handle.raw_handle(), lib.notification_sender().await);
                let task_ctx = &mut Context::from_waker(&waker);
                let res = match Pin::new(&mut on_signals).poll(task_ctx) {
                    Poll::Ready(res) => res,
                    Poll::Pending => Err(zx_status::Status::SHOULD_WAIT),
                };
                // Prevent the handle from closing prematurely.
                std::mem::forget(handle);
                responder.send(res).unwrap();
            }
            Self::SocketCreate { options, responder } => {
                match options {
                    fidl::SocketOpts::STREAM => {
                        responder.send(fidl::Socket::create_stream()).unwrap();
                    }
                    fidl::SocketOpts::DATAGRAM => {
                        responder.send(fidl::Socket::create_datagram()).unwrap();
                    }
                };
            }
            Self::SocketRead { lib, socket, mut out_buf, responder } => {
                let socket = fidl::AsyncSocket::from_socket(socket);
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
            Self::TargetAdd { env, addr, scope_id, port, wait, responder } => {
                let res = match env.target_add(addr, scope_id, port, wait).await {
                    Ok(_) => zx_status::Status::OK,
                    Err(e) => {
                        env.write_err(e);
                        zx_status::Status::INTERNAL
                    }
                };
                responder.send(res).unwrap();
            }
            Self::TargetWait { env, timeout: timeout_float, responder } => {
                let target = match env.target_proxy_factory().await {
                    Ok(t) => t,
                    Err(e) => {
                        env.write_err(e);
                        responder.send(zx_status::Status::INTERNAL).unwrap();
                        return;
                    }
                };
                let default_target = match ffx_target::resolve_default_target(&env.context).await {
                    Ok(t) => t,
                    Err(e) => {
                        env.write_err(e);
                        responder.send(zx_status::Status::INTERNAL).unwrap();
                        return;
                    }
                };
                let knock_fut = async {
                    loop {
                        break match knock_target(&target).await {
                            Ok(()) => Ok(()),
                            Err(KnockError::NonCriticalError(_)) => continue,
                            Err(KnockError::CriticalError(e)) => Err(e),
                        };
                    }
                };
                let err = match timeout(Duration::from_secs_f64(timeout_float), knock_fut).await {
                    Ok(res) => match res {
                        Ok(()) => {
                            responder.send(zx_status::Status::OK).unwrap();
                            return;
                        }
                        Err(e) => e,
                    },
                    Err(e) => {
                        anyhow::anyhow!(
                            "timeout attempting to knock target {} {}: {:?}",
                            default_target
                                .as_ref()
                                .map(ToString::to_string)
                                .unwrap_or("unspecified".to_owned()),
                            if default_target.is_none() { "(default)" } else { "" },
                            e
                        )
                    }
                };
                env.write_err(err);
                responder.send(zx_status::Status::INTERNAL).unwrap();
            }
        }
    }
}
