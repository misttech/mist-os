// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;

#[fuchsia::test]
async fn socket() {
    let mut fdomain = FDomain::new_empty();

    let hid_socket_write = 0;
    let hid_socket_read = 2;

    assert!(fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [
                proto::NewHandleId { id: hid_socket_write },
                proto::NewHandleId { id: hid_socket_read },
            ]
        })
        .is_ok());

    let result = fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [
                proto::NewHandleId { id: hid_socket_write },
                proto::NewHandleId { id: hid_socket_read },
            ],
        })
        .err()
        .unwrap();

    let proto::Error::NewHandleIdReused(proto::NewHandleIdReused { id: i, same_call: false }) =
        result
    else {
        panic!();
    };

    assert!(hid_socket_write == i || hid_socket_read == i);

    let tid_write = 42.try_into().unwrap();
    let tid_read = 89.try_into().unwrap();

    let data =
        b"I've got to admit, I've been giving no small amount of thought to these walls.".to_vec();
    let data_compare = data.clone();
    let data_len = data.len();

    fdomain.write_socket(
        tid_write,
        proto::SocketWriteSocketRequest { handle: proto::HandleId { id: hid_socket_write }, data },
    );
    fdomain.read_socket(
        tid_read,
        proto::SocketReadSocketRequest {
            handle: proto::HandleId { id: hid_socket_read },
            max_bytes: 1024,
        },
    );

    let response_write = fdomain.next().await.unwrap();

    let FDomainEvent::WroteSocket(got_tid, Ok(proto::SocketWriteSocketResponse { wrote })) =
        response_write
    else {
        panic!();
    };

    assert_eq!(tid_write, got_tid);
    assert_eq!(data_len, wrote.try_into().unwrap());

    let response_read = fdomain.next().await.unwrap();

    let FDomainEvent::SocketData(got_tid, Ok(proto::SocketReadSocketResponse { data: got_data })) =
        response_read
    else {
        panic!();
    };

    assert_eq!(tid_read, got_tid);
    assert_eq!(data_compare, got_data);

    let tid_close = 420.try_into().unwrap();
    fdomain.close(
        tid_close,
        proto::FDomainCloseRequest { handles: vec![proto::HandleId { id: hid_socket_read }] },
    );
    let response_close = fdomain.next().await.unwrap();
    let FDomainEvent::ClosedHandle(got_tid, Ok(())) = response_close else {
        panic!();
    };
    assert_eq!(tid_close, got_tid);

    let tid_write_fail = 101.try_into().unwrap();

    fdomain.write_socket(
        tid_write_fail,
        proto::SocketWriteSocketRequest {
            handle: proto::HandleId { id: hid_socket_write },
            data: b"greeble".to_vec(),
        },
    );

    let response_write = fdomain.next().await.unwrap();
    let FDomainEvent::WroteSocket(got_tid, Err(proto::WriteSocketError { wrote, error })) =
        response_write
    else {
        panic!();
    };

    assert_eq!(tid_write_fail, got_tid);
    assert_eq!(0, wrote);
    let proto::Error::TargetError(err) = error else {
        panic!();
    };
    assert_eq!(fidl::Status::PEER_CLOSED.into_raw(), err);

    let tid_write_fail_2nd = 102.try_into().unwrap();

    fdomain.write_socket(
        tid_write_fail_2nd,
        proto::SocketWriteSocketRequest {
            handle: proto::HandleId { id: hid_socket_write },
            data: b"greeble".to_vec(),
        },
    );

    let response_write = fdomain.next().await.unwrap();
    let FDomainEvent::WroteSocket(got_tid, Err(proto::WriteSocketError { wrote, error })) =
        response_write
    else {
        panic!();
    };

    assert_eq!(tid_write_fail_2nd, got_tid);
    assert_eq!(0, wrote);
    assert!(matches!(error, proto::Error::ErrorPending(proto::ErrorPending)));

    fdomain
        .acknowledge_write_error(proto::FDomainAcknowledgeWriteErrorRequest {
            handle: proto::HandleId { id: hid_socket_write },
        })
        .unwrap();

    let tid_write_fail_3rd = 103.try_into().unwrap();

    fdomain.write_socket(
        tid_write_fail_3rd,
        proto::SocketWriteSocketRequest {
            handle: proto::HandleId { id: hid_socket_write },
            data: b"greeble".to_vec(),
        },
    );

    let response_write = fdomain.next().await.unwrap();
    let FDomainEvent::WroteSocket(got_tid, Err(proto::WriteSocketError { wrote, error })) =
        response_write
    else {
        panic!();
    };

    assert_eq!(tid_write_fail_3rd, got_tid);
    assert_eq!(0, wrote);
    let proto::Error::TargetError(err) = error else {
        panic!();
    };
    assert_eq!(fidl::Status::PEER_CLOSED.into_raw(), err);

    let tid_read_fail = 105.try_into().unwrap();
    fdomain.read_socket(
        tid_read_fail,
        proto::SocketReadSocketRequest {
            handle: proto::HandleId { id: hid_socket_read },
            max_bytes: 1024,
        },
    );

    let response_read = fdomain.next().await.unwrap();

    let FDomainEvent::SocketData(
        got_tid,
        Err(proto::Error::BadHandleId(proto::BadHandleId { id: got_id })),
    ) = response_read
    else {
        panic!();
    };

    assert_eq!(tid_read_fail, got_tid);
    assert_eq!(hid_socket_read, got_id);
}

#[fuchsia::test]
async fn channel() {
    let mut fdomain = FDomain::new_empty();

    let hid_channel_write = 0;
    let hid_channel_read = 2;

    assert!(fdomain
        .create_channel(proto::ChannelCreateChannelRequest {
            handles: [
                proto::NewHandleId { id: hid_channel_write },
                proto::NewHandleId { id: hid_channel_read },
            ]
        })
        .is_ok());

    let result = fdomain
        .create_channel(proto::ChannelCreateChannelRequest {
            handles: [
                proto::NewHandleId { id: hid_channel_write },
                proto::NewHandleId { id: hid_channel_read },
            ],
        })
        .err()
        .unwrap();

    let proto::Error::NewHandleIdReused(proto::NewHandleIdReused { id: i, same_call: false }) =
        result
    else {
        panic!();
    };

    assert!(hid_channel_write == i || hid_channel_read == i);

    let hid_passing_socket_a = 4;
    let hid_passing_socket_b = 6;

    assert!(fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [
                proto::NewHandleId { id: hid_passing_socket_a },
                proto::NewHandleId { id: hid_passing_socket_b },
            ]
        })
        .is_ok());

    let tid_write = 42.try_into().unwrap();
    let tid_read = 89.try_into().unwrap();

    let data =
        b"I've got to admit, I've been giving no small amount of thought to these walls.".to_vec();
    let data_compare = data.clone();

    fdomain.write_channel(
        tid_write,
        proto::ChannelWriteChannelRequest {
            handle: proto::HandleId { id: hid_channel_write },
            handles: proto::Handles::Handles(vec![proto::HandleId { id: hid_passing_socket_a }]),
            data,
        },
    );
    fdomain.read_channel(
        tid_read,
        proto::ChannelReadChannelRequest { handle: proto::HandleId { id: hid_channel_read } },
    );

    let response_write = fdomain.next().await.unwrap();

    let FDomainEvent::WroteChannel(got_tid, Ok(())) = response_write else {
        panic!();
    };

    assert_eq!(tid_write, got_tid);

    let response_read = fdomain.next().await.unwrap();

    let FDomainEvent::ChannelData(got_tid, Ok(mut got_message)) = response_read else {
        panic!();
    };

    assert_eq!(tid_read, got_tid);
    assert_eq!(data_compare, got_message.data);
    assert_eq!(1, got_message.handles.len());
    let got_handle = got_message.handles.pop().unwrap();

    assert_ne!(0, got_handle.handle.id & (1 << 31));
    assert_eq!(fidl::ObjectType::SOCKET, got_handle.type_);
    assert_eq!(
        fidl::Rights::DUPLICATE
            | fidl::Rights::TRANSFER
            | fidl::Rights::READ
            | fidl::Rights::WRITE
            | fidl::Rights::GET_PROPERTY
            | fidl::Rights::SET_PROPERTY
            | fidl::Rights::SIGNAL
            | fidl::Rights::SIGNAL_PEER
            | fidl::Rights::WAIT
            | fidl::Rights::INSPECT
            | fidl::Rights::MANAGE_SOCKET,
        got_handle.rights
    );

    let tid_read_fail = 100.try_into().unwrap();
    fdomain.read_socket(
        tid_read_fail,
        proto::SocketReadSocketRequest {
            handle: proto::HandleId { id: hid_passing_socket_a },
            max_bytes: 1024,
        },
    );

    let response_read = fdomain.next().await.unwrap();

    let FDomainEvent::SocketData(
        got_tid,
        Err(proto::Error::BadHandleId(proto::BadHandleId { id: got_id })),
    ) = response_read
    else {
        panic!();
    };

    assert_eq!(tid_read_fail, got_tid);
    assert_eq!(hid_passing_socket_a, got_id);

    let socket_data = b"Flappy Jangos!".to_vec();
    let socket_data_compare = socket_data.clone();
    let tid_write_socket = 110.try_into().unwrap();
    fdomain.write_socket(
        tid_write_socket,
        proto::SocketWriteSocketRequest {
            handle: proto::HandleId { id: hid_passing_socket_b },
            data: socket_data,
        },
    );

    let response_write = fdomain.next().await.unwrap();

    let FDomainEvent::WroteSocket(got_tid, Ok(proto::SocketWriteSocketResponse { wrote })) =
        response_write
    else {
        panic!();
    };

    assert_eq!(tid_write_socket, got_tid);
    assert_eq!(socket_data_compare.len(), wrote.try_into().unwrap());

    let tid_read_succeed = 111.try_into().unwrap();
    fdomain.read_socket(
        tid_read_succeed,
        proto::SocketReadSocketRequest { handle: got_handle.handle, max_bytes: 1024 },
    );

    let response_read = fdomain.next().await.unwrap();

    let FDomainEvent::SocketData(got_tid, Ok(proto::SocketReadSocketResponse { data: got_data })) =
        response_read
    else {
        panic!();
    };

    assert_eq!(tid_read_succeed, got_tid);
    assert_eq!(socket_data_compare, got_data);

    let tid_close = 420.try_into().unwrap();
    fdomain.close(
        tid_close,
        proto::FDomainCloseRequest { handles: vec![proto::HandleId { id: hid_channel_read }] },
    );
    let response_close = fdomain.next().await.unwrap();
    let FDomainEvent::ClosedHandle(got_tid, Ok(())) = response_close else {
        panic!();
    };
    assert_eq!(tid_close, got_tid);

    let tid_write_fail = 101.try_into().unwrap();

    fdomain.write_channel(
        tid_write_fail,
        proto::ChannelWriteChannelRequest {
            handle: proto::HandleId { id: hid_channel_write },
            handles: proto::Handles::Handles(vec![]),
            data: b"greeble".to_vec(),
        },
    );

    let response_write = fdomain.next().await.unwrap();
    let FDomainEvent::WroteChannel(
        got_tid,
        Err(proto::WriteChannelError::Error(proto::Error::TargetError(err))),
    ) = response_write
    else {
        panic!();
    };

    assert_eq!(tid_write_fail, got_tid);
    assert_eq!(fidl::Status::PEER_CLOSED.into_raw(), err);

    let tid_write_fail_2nd = 102.try_into().unwrap();

    fdomain.write_channel(
        tid_write_fail_2nd,
        proto::ChannelWriteChannelRequest {
            handle: proto::HandleId { id: hid_channel_write },
            handles: proto::Handles::Handles(vec![]),
            data: b"greeble".to_vec(),
        },
    );

    let response_write = fdomain.next().await.unwrap();
    let FDomainEvent::WroteChannel(
        got_tid,
        Err(proto::WriteChannelError::Error(proto::Error::ErrorPending(proto::ErrorPending))),
    ) = response_write
    else {
        panic!();
    };

    assert_eq!(tid_write_fail_2nd, got_tid);

    fdomain
        .acknowledge_write_error(proto::FDomainAcknowledgeWriteErrorRequest {
            handle: proto::HandleId { id: hid_channel_write },
        })
        .unwrap();

    let tid_write_fail_3rd = 103.try_into().unwrap();

    fdomain.write_channel(
        tid_write_fail_3rd,
        proto::ChannelWriteChannelRequest {
            handle: proto::HandleId { id: hid_channel_write },
            handles: proto::Handles::Handles(vec![]),
            data: b"greeble".to_vec(),
        },
    );

    let response_write = fdomain.next().await.unwrap();
    let FDomainEvent::WroteChannel(
        got_tid,
        Err(proto::WriteChannelError::Error(proto::Error::TargetError(err))),
    ) = response_write
    else {
        panic!();
    };

    assert_eq!(tid_write_fail_3rd, got_tid);
    assert_eq!(fidl::Status::PEER_CLOSED.into_raw(), err);

    let tid_read_fail = 105.try_into().unwrap();
    fdomain.read_channel(
        tid_read_fail,
        proto::ChannelReadChannelRequest { handle: proto::HandleId { id: hid_channel_read } },
    );

    let response_read = fdomain.next().await.unwrap();

    let FDomainEvent::ChannelData(
        got_tid,
        Err(proto::Error::BadHandleId(proto::BadHandleId { id: got_id })),
    ) = response_read
    else {
        panic!();
    };

    assert_eq!(tid_read_fail, got_tid);
    assert_eq!(hid_channel_read, got_id);
}

#[fuchsia::test]
async fn bad_id() {
    let mut fdomain = FDomain::new_empty();

    let hid_socket_write = 0;
    // This is a server-side handle ID.
    let hid_socket_read = 1 << 31;

    let result = fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [
                proto::NewHandleId { id: hid_socket_write },
                proto::NewHandleId { id: hid_socket_read },
            ],
        })
        .err()
        .unwrap();

    let proto::Error::NewHandleIdOutOfRange(proto::NewHandleIdOutOfRange { id: i }) = result else {
        panic!();
    };

    assert_eq!(hid_socket_read, i);
}

#[fuchsia::test]
async fn bad_channel_writes() {
    let mut fdomain = FDomain::new_empty();

    let channel_hid_a = 0;
    let channel_hid_b = 2;
    let garbage_hid = 428;
    let tid = 42.try_into().unwrap();

    assert!(fdomain
        .create_channel(proto::ChannelCreateChannelRequest {
            handles: [
                proto::NewHandleId { id: channel_hid_a },
                proto::NewHandleId { id: channel_hid_b },
            ],
        })
        .is_ok());

    fdomain.write_channel(
        tid,
        proto::ChannelWriteChannelRequest {
            handle: proto::HandleId { id: channel_hid_a },
            data: vec![],
            handles: proto::Handles::Handles(vec![proto::HandleId { id: garbage_hid }]),
        },
    );

    let response_write = fdomain.next().await.unwrap();
    let FDomainEvent::WroteChannel(got_tid, Err(proto::WriteChannelError::OpErrors(mut errs))) =
        response_write
    else {
        panic!();
    };

    assert_eq!(tid, got_tid);
    assert_eq!(1, errs.len());
    let err = *errs.pop().unwrap().unwrap();
    let proto::Error::BadHandleId(proto::BadHandleId { id: got_id }) = err else {
        panic!();
    };
    assert_eq!(garbage_hid, got_id);

    assert!(fdomain
        .acknowledge_write_error(proto::FDomainAcknowledgeWriteErrorRequest {
            handle: proto::HandleId { id: channel_hid_a }
        })
        .is_ok());

    let socket_hid_a = 4;
    let socket_hid_b = 6;
    let socket_hid_c = 8;
    let tid = 43.try_into().unwrap();

    assert!(fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [
                proto::NewHandleId { id: socket_hid_a },
                proto::NewHandleId { id: socket_hid_b },
            ],
        })
        .is_ok());

    let tid_replace = 420.try_into().unwrap();
    fdomain
        .replace(
            tid_replace,
            proto::FDomainReplaceRequest {
                handle: proto::HandleId { id: socket_hid_b },
                new_handle: proto::NewHandleId { id: socket_hid_c },
                rights: fidl::Rights::READ,
            },
        )
        .unwrap();
    let response_replace = fdomain.next().await.unwrap();
    let FDomainEvent::ReplacedHandle(got_tid, Ok(())) = response_replace else {
        panic!();
    };
    assert_eq!(tid_replace, got_tid);

    fdomain.write_channel(
        tid,
        proto::ChannelWriteChannelRequest {
            handle: proto::HandleId { id: channel_hid_a },
            data: vec![],
            handles: proto::Handles::Dispositions(vec![proto::HandleDisposition {
                handle: proto::HandleOp::Duplicate(proto::HandleId { id: socket_hid_c }),
                rights: fidl::Rights::SAME_RIGHTS,
            }]),
        },
    );

    let response_write = fdomain.next().await.unwrap();
    let FDomainEvent::WroteChannel(got_tid, Err(proto::WriteChannelError::OpErrors(mut errs))) =
        response_write
    else {
        panic!();
    };

    assert_eq!(tid, got_tid);
    assert_eq!(1, errs.len());
    let err = *errs.pop().unwrap().unwrap();
    let proto::Error::TargetError(e) = err else {
        panic!();
    };

    assert_eq!(fidl::Status::ACCESS_DENIED.into_raw(), e);

    assert!(fdomain
        .acknowledge_write_error(proto::FDomainAcknowledgeWriteErrorRequest {
            handle: proto::HandleId { id: channel_hid_a }
        })
        .is_ok());

    let tid = 44.try_into().unwrap();
    fdomain.write_channel(
        tid,
        proto::ChannelWriteChannelRequest {
            handle: proto::HandleId { id: channel_hid_a },
            data: vec![],
            handles: proto::Handles::Handles(vec![proto::HandleId { id: channel_hid_a }]),
        },
    );

    let response_write = fdomain.next().await.unwrap();
    let FDomainEvent::WroteChannel(got_tid, Err(proto::WriteChannelError::OpErrors(mut errs))) =
        response_write
    else {
        panic!();
    };

    assert_eq!(tid, got_tid);
    assert_eq!(1, errs.len());
    let err = *errs.pop().unwrap().unwrap();
    let proto::Error::WroteToSelf(proto::WroteToSelf) = err else {
        panic!();
    };
}

#[fuchsia::test]
async fn event_signal() {
    let mut fdomain = FDomain::new_empty();
    let event_hid = 0;
    let tid = 42.try_into().unwrap();

    assert!(fdomain
        .create_event(proto::EventCreateEventRequest {
            handle: proto::NewHandleId { id: event_hid }
        })
        .is_ok());

    let event_hid = proto::HandleId { id: event_hid };

    assert!(fdomain
        .signal(proto::FDomainSignalRequest {
            handle: event_hid,
            set: fidl::Signals::USER_5.bits(),
            clear: fidl::Signals::empty().bits(),
        })
        .is_ok());

    fdomain.wait_for_signals(
        tid,
        proto::FDomainWaitForSignalsRequest {
            handle: event_hid,
            signals: fidl::Signals::USER_5.bits(),
        },
    );

    let FDomainEvent::WaitForSignals(got_tid, Ok(signals)) = fdomain.next().await.unwrap() else {
        panic!()
    };

    assert_eq!(tid, got_tid);
    assert_eq!(fidl::Signals::USER_5, fidl::Signals::from_bits_retain(signals.signals));
}

#[fuchsia::test]
async fn eventpair_signal() {
    let mut fdomain = FDomain::new_empty();
    let event_hid_a = 0;
    let event_hid_b = 2;
    let tid = 42.try_into().unwrap();

    assert!(fdomain
        .create_event_pair(proto::EventPairCreateEventPairRequest {
            handles: [
                proto::NewHandleId { id: event_hid_a },
                proto::NewHandleId { id: event_hid_b }
            ]
        })
        .is_ok());

    let event_hid_a = proto::HandleId { id: event_hid_a };
    let event_hid_b = proto::HandleId { id: event_hid_b };

    assert!(fdomain
        .signal_peer(proto::FDomainSignalPeerRequest {
            handle: event_hid_a,
            set: fidl::Signals::USER_5.bits(),
            clear: fidl::Signals::empty().bits(),
        })
        .is_ok());

    fdomain.wait_for_signals(
        tid,
        proto::FDomainWaitForSignalsRequest {
            handle: event_hid_b,
            signals: fidl::Signals::USER_5.bits(),
        },
    );

    let FDomainEvent::WaitForSignals(got_tid, Ok(signals)) = fdomain.next().await.unwrap() else {
        panic!()
    };

    assert_eq!(tid, got_tid);
    assert_eq!(fidl::Signals::USER_5, fidl::Signals::from_bits_retain(signals.signals));
}

#[fuchsia::test]
async fn duplicate_new_handles_fails() {
    let mut fdomain = FDomain::new_empty();
    let event_hid_a = 0;
    let event_hid_b = 0;

    let result = fdomain.create_event_pair(proto::EventPairCreateEventPairRequest {
        handles: [proto::NewHandleId { id: event_hid_a }, proto::NewHandleId { id: event_hid_b }],
    });

    let Err(proto::Error::NewHandleIdReused(proto::NewHandleIdReused {
        id: got_id,
        same_call: true,
    })) = result
    else {
        panic!()
    };
    assert_eq!(event_hid_a, got_id);
}

#[fuchsia::test]
async fn duplicate_socket() {
    let mut fdomain = FDomain::new_empty();
    let hid_a = 0;
    let hid_b = 2;
    let hid_c = 4;

    assert!(fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [proto::NewHandleId { id: hid_a }, proto::NewHandleId { id: hid_b },],
        })
        .is_ok());

    let hid_a = proto::HandleId { id: hid_a };
    let hid_b = proto::HandleId { id: hid_b };

    assert!(fdomain
        .duplicate(proto::FDomainDuplicateRequest {
            handle: hid_a,
            new_handle: proto::NewHandleId { id: hid_c },
            rights: fidl::Rights::SAME_RIGHTS,
        })
        .is_ok());

    let hid_c = proto::HandleId { id: hid_c };

    let tid_1 = 1.try_into().unwrap();
    let tid_2 = 2.try_into().unwrap();
    let tid_3 = 3.try_into().unwrap();
    let tid_4 = 4.try_into().unwrap();

    let data_a = b"Feral cats move in mysterious ways";
    let data_b = b"A Moistness of the Soul";

    fdomain.write_socket(
        tid_1,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_a.to_vec() },
    );
    fdomain.write_socket(
        tid_2,
        proto::SocketWriteSocketRequest { handle: hid_c, data: data_b.to_vec() },
    );
    fdomain.read_socket(tid_3, proto::SocketReadSocketRequest { handle: hid_b, max_bytes: 1024 });
    fdomain.read_socket(tid_4, proto::SocketReadSocketRequest { handle: hid_b, max_bytes: 1024 });

    // 1) We don't know which order the data will be written in, because the
    //    FDomain spec doesn't require a strong ordering of operations
    //    across different handles.
    // 2) We don't know if the two writes will be concatenated in the buffer
    //    or sent to us in one big read, because we don't control the timing
    //    involved.

    let mut wrote_socket_events = Vec::with_capacity(2);
    let mut socket_data_events = Vec::with_capacity(2);

    loop {
        match fdomain.next().await.unwrap() {
            FDomainEvent::WroteSocket(
                got_tid,
                Ok(proto::SocketWriteSocketResponse { wrote: wrote_actual }),
            ) => wrote_socket_events.push((got_tid, wrote_actual)),

            FDomainEvent::SocketData(
                got_tid,
                Ok(proto::SocketReadSocketResponse { data: got_data }),
            ) => {
                let len = got_data.len();
                socket_data_events.push((got_tid, got_data));

                if socket_data_events.len() == 2 || len >= data_a.len() + data_b.len() {
                    break;
                }
            }
            other => panic!("Got unexpected event {other:?}"),
        }
    }

    assert_eq!(2, wrote_socket_events.len());

    if wrote_socket_events[0].0 != tid_1 {
        let (a, b) = wrote_socket_events.split_at_mut(1);
        std::mem::swap(&mut a[0], &mut b[0]);
    }

    let (got_tid, wrote_actual) = wrote_socket_events.remove(0);
    assert_eq!(tid_1, got_tid);
    assert_eq!(data_a.len(), wrote_actual.try_into().unwrap());
    let (got_tid, wrote_actual) = wrote_socket_events.remove(0);
    assert_eq!(tid_2, got_tid);
    assert_eq!(data_b.len(), wrote_actual.try_into().unwrap());

    let (got_tid, got_data) = socket_data_events.remove(0);
    assert_eq!(tid_3, got_tid);

    // 1) We don't know which order the data will be written in, because the
    //    FDomain spec doesn't require a strong ordering of operations
    //    across different handles.
    // 2) We don't know if the two writes will be concatenated in the buffer
    //    or sent to us in one big read, because we don't control the timing
    //    involved.
    let remaining: &[u8] = if got_data == data_a {
        data_b
    } else if got_data == data_b {
        data_a
    } else {
        assert!(got_data.len() == data_a.len() + data_b.len());
        if got_data.starts_with(data_a) {
            assert!(got_data.ends_with(data_b));
        } else {
            assert!(got_data.starts_with(data_b));
            assert!(got_data.ends_with(data_a));
        }
        return;
    };

    let (got_tid, got_data) = socket_data_events.remove(0);
    assert_eq!(tid_4, got_tid);
    assert_eq!(remaining, got_data);
}

#[fuchsia::test]
async fn socket_disposition() {
    let mut fdomain = FDomain::new_empty();
    let hid_a = 0;
    let hid_b = 2;

    assert!(fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [proto::NewHandleId { id: hid_a }, proto::NewHandleId { id: hid_b },],
        })
        .is_ok());

    let hid_a = proto::HandleId { id: hid_a };

    let tid_1 = 1.try_into().unwrap();
    let tid_2 = 2.try_into().unwrap();
    let tid_3 = 3.try_into().unwrap();

    let data_a = b"The ghosts o' robot dinosaurs? Cor!";
    let data_b = b"An educational robot vaudeville show about quantum mechanics";

    fdomain.write_socket(
        tid_1,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_a.to_vec() },
    );

    fdomain.set_socket_disposition(
        tid_2,
        proto::SocketSetSocketDispositionRequest {
            handle: hid_a,
            disposition: proto::SocketDisposition::WriteDisabled,
            disposition_peer: proto::SocketDisposition::NoChange,
        },
    );

    fdomain.write_socket(
        tid_3,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_b.to_vec() },
    );

    let FDomainEvent::WroteSocket(
        got_tid,
        Ok(proto::SocketWriteSocketResponse { wrote: wrote_actual }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_1, got_tid);
    assert_eq!(data_a.len(), wrote_actual.try_into().unwrap());

    let FDomainEvent::SocketDispositionSet(got_tid, Ok(())) = fdomain.next().await.unwrap() else {
        panic!()
    };
    assert_eq!(tid_2, got_tid);

    let FDomainEvent::WroteSocket(
        got_tid,
        Err(proto::WriteSocketError { error, wrote: wrote_actual }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    let proto::Error::TargetError(error) = error else { panic!() };
    assert_eq!(tid_3, got_tid);
    assert_eq!(fidl::Status::BAD_STATE.into_raw(), error);
    assert_eq!(0, wrote_actual);
}

#[fuchsia::test]
async fn socket_disposition_peer() {
    let mut fdomain = FDomain::new_empty();
    let hid_a = 0;
    let hid_b = 2;

    assert!(fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [proto::NewHandleId { id: hid_a }, proto::NewHandleId { id: hid_b },],
        })
        .is_ok());

    let hid_a = proto::HandleId { id: hid_a };
    let hid_b = proto::HandleId { id: hid_b };

    let tid_1 = 1.try_into().unwrap();
    let tid_2 = 2.try_into().unwrap();
    let tid_3 = 3.try_into().unwrap();

    let data_a = b"The ghosts o' robot dinosaurs? Cor!";
    let data_b = b"An educational robot vaudeville show about quantum mechanics";

    fdomain.write_socket(
        tid_1,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_a.to_vec() },
    );

    let FDomainEvent::WroteSocket(
        got_tid,
        Ok(proto::SocketWriteSocketResponse { wrote: wrote_actual }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_1, got_tid);
    assert_eq!(data_a.len(), wrote_actual.try_into().unwrap());

    fdomain.set_socket_disposition(
        tid_2,
        proto::SocketSetSocketDispositionRequest {
            handle: hid_b,
            disposition: proto::SocketDisposition::NoChange,
            disposition_peer: proto::SocketDisposition::WriteDisabled,
        },
    );

    let FDomainEvent::SocketDispositionSet(got_tid, Ok(())) = fdomain.next().await.unwrap() else {
        panic!()
    };
    assert_eq!(tid_2, got_tid);

    fdomain.write_socket(
        tid_3,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_b.to_vec() },
    );

    let FDomainEvent::WroteSocket(
        got_tid,
        Err(proto::WriteSocketError { error, wrote: wrote_actual }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    let proto::Error::TargetError(error) = error else { panic!() };
    assert_eq!(tid_3, got_tid);
    assert_eq!(fidl::Status::BAD_STATE.into_raw(), error);
    assert_eq!(0, wrote_actual);
}

#[fuchsia::test]
async fn socket_async_read() {
    let mut fdomain = FDomain::new_empty();
    let hid_a = 0;
    let hid_b = 2;

    assert!(fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [proto::NewHandleId { id: hid_a }, proto::NewHandleId { id: hid_b },],
        })
        .is_ok());

    let hid_a = proto::HandleId { id: hid_a };
    let hid_b = proto::HandleId { id: hid_b };

    let tid_1 = 1.try_into().unwrap();
    let tid_2 = 2.try_into().unwrap();
    let tid_3 = 3.try_into().unwrap();

    let data_a = b"The ghosts o' robot dinosaurs? Cor!";
    let data_b = b"An educational robot vaudeville show about quantum mechanics";

    fdomain.write_socket(
        tid_1,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_a.to_vec() },
    );

    let FDomainEvent::WroteSocket(
        got_tid,
        Ok(proto::SocketWriteSocketResponse { wrote: wrote_actual }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_1, got_tid);
    assert_eq!(data_a.len(), wrote_actual.try_into().unwrap());

    fdomain.read_socket_streaming_start(
        tid_2,
        proto::SocketReadSocketStreamingStartRequest { handle: hid_b },
    );

    let FDomainEvent::SocketStreamingReadStart(got_tid, Ok(())) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_2, got_tid);

    let FDomainEvent::SocketStreamingData(proto::SocketOnSocketStreamingDataRequest {
        handle: got_handle,
        socket_message: proto::SocketMessage::Data(got_data),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(hid_b, got_handle);
    assert_eq!(data_a, got_data.as_slice());

    fdomain.write_socket(
        tid_3,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_b.to_vec() },
    );

    let FDomainEvent::WroteSocket(
        got_tid,
        Ok(proto::SocketWriteSocketResponse { wrote: wrote_actual }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_3, got_tid);
    assert_eq!(data_b.len(), wrote_actual.try_into().unwrap());

    let FDomainEvent::SocketStreamingData(proto::SocketOnSocketStreamingDataRequest {
        handle: got_handle,
        socket_message: proto::SocketMessage::Data(got_data),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(hid_b, got_handle);
    assert_eq!(data_b, got_data.as_slice());
}

#[fuchsia::test]
async fn socket_async_read_detect_close() {
    let mut fdomain = FDomain::new_empty();
    let hid_a = 0;
    let hid_b = 2;

    assert!(fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [proto::NewHandleId { id: hid_a }, proto::NewHandleId { id: hid_b },],
        })
        .is_ok());

    let hid_a = proto::HandleId { id: hid_a };
    let hid_b = proto::HandleId { id: hid_b };

    let tid_1 = 1.try_into().unwrap();
    let tid_2 = 2.try_into().unwrap();
    let tid_3 = 3.try_into().unwrap();

    let data_a = b"The ghosts o' robot dinosaurs? Cor!";

    fdomain.write_socket(
        tid_1,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_a.to_vec() },
    );

    let FDomainEvent::WroteSocket(
        got_tid,
        Ok(proto::SocketWriteSocketResponse { wrote: wrote_actual }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_1, got_tid);
    assert_eq!(data_a.len(), wrote_actual.try_into().unwrap());

    fdomain.read_socket_streaming_start(
        tid_2,
        proto::SocketReadSocketStreamingStartRequest { handle: hid_b },
    );

    let FDomainEvent::SocketStreamingReadStart(got_tid, Ok(())) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_2, got_tid);

    let FDomainEvent::SocketStreamingData(proto::SocketOnSocketStreamingDataRequest {
        handle: got_handle,
        socket_message: proto::SocketMessage::Data(got_data),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(hid_b, got_handle);
    assert_eq!(data_a, got_data.as_slice());

    fdomain.close(tid_3, proto::FDomainCloseRequest { handles: vec![hid_a] });

    let FDomainEvent::ClosedHandle(got_tid, Ok(())) = fdomain.next().await.unwrap() else {
        panic!()
    };
    assert_eq!(tid_3, got_tid);

    let FDomainEvent::SocketStreamingData(proto::SocketOnSocketStreamingDataRequest {
        handle: got_handle,
        socket_message: proto::SocketMessage::Stopped(proto::AioStopped { error: Some(err) }),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(*err, proto::Error::TargetError(fidl::Status::PEER_CLOSED.into_raw()));
    assert_eq!(hid_b, got_handle);
}

#[fuchsia::test]
async fn socket_async_read_stop() {
    let mut fdomain = FDomain::new_empty();
    let hid_a = 0;
    let hid_b = 2;

    assert!(fdomain
        .create_socket(proto::SocketCreateSocketRequest {
            options: proto::SocketType::Stream,
            handles: [proto::NewHandleId { id: hid_a }, proto::NewHandleId { id: hid_b },],
        })
        .is_ok());

    let hid_a = proto::HandleId { id: hid_a };
    let hid_b = proto::HandleId { id: hid_b };

    let tid_1 = 1.try_into().unwrap();
    let tid_2 = 2.try_into().unwrap();
    let tid_3 = 3.try_into().unwrap();
    let tid_4 = 4.try_into().unwrap();
    let tid_5 = 5.try_into().unwrap();

    let data_a = b"The ghosts o' robot dinosaurs? Cor!";
    let data_b = b"An educational robot vaudeville show about quantum mechanics";

    fdomain.write_socket(
        tid_1,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_a.to_vec() },
    );

    let FDomainEvent::WroteSocket(
        got_tid,
        Ok(proto::SocketWriteSocketResponse { wrote: wrote_actual }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_1, got_tid);
    assert_eq!(data_a.len(), wrote_actual.try_into().unwrap());

    fdomain.read_socket_streaming_start(
        tid_2,
        proto::SocketReadSocketStreamingStartRequest { handle: hid_b },
    );

    let FDomainEvent::SocketStreamingReadStart(got_tid, Ok(())) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_2, got_tid);

    let FDomainEvent::SocketStreamingData(proto::SocketOnSocketStreamingDataRequest {
        handle: got_handle,
        socket_message: proto::SocketMessage::Data(got_data),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(hid_b, got_handle);
    assert_eq!(data_a, got_data.as_slice());

    fdomain.read_socket_streaming_stop(
        tid_3,
        proto::SocketReadSocketStreamingStopRequest { handle: hid_b },
    );

    let FDomainEvent::SocketStreamingReadStop(got_tid, Ok(())) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_3, got_tid);

    fdomain.write_socket(
        tid_4,
        proto::SocketWriteSocketRequest { handle: hid_a, data: data_b.to_vec() },
    );

    let FDomainEvent::WroteSocket(
        got_tid,
        Ok(proto::SocketWriteSocketResponse { wrote: wrote_actual }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_4, got_tid);
    assert_eq!(data_b.len(), wrote_actual.try_into().unwrap());

    fdomain.read_socket(tid_5, proto::SocketReadSocketRequest { handle: hid_b, max_bytes: 4096 });

    let FDomainEvent::SocketData(got_tid, Ok(proto::SocketReadSocketResponse { data })) =
        fdomain.next().await.unwrap()
    else {
        panic!()
    };

    assert_eq!(tid_5, got_tid);
    assert_eq!(data_b, data.as_slice());
}

#[fuchsia::test]
async fn channel_async_read() {
    let mut fdomain = FDomain::new_empty();
    let hid_a = 0;
    let hid_b = 2;

    assert!(fdomain
        .create_channel(proto::ChannelCreateChannelRequest {
            handles: [proto::NewHandleId { id: hid_a }, proto::NewHandleId { id: hid_b },],
        })
        .is_ok());

    let hid_a = proto::HandleId { id: hid_a };
    let hid_b = proto::HandleId { id: hid_b };

    let tid_1 = 1.try_into().unwrap();
    let tid_2 = 2.try_into().unwrap();
    let tid_3 = 3.try_into().unwrap();

    let data_a = b"The ghosts o' robot dinosaurs? Cor!";
    let data_b = b"An educational robot vaudeville show about quantum mechanics";

    fdomain.write_channel(
        tid_1,
        proto::ChannelWriteChannelRequest {
            handle: hid_a,
            data: data_a.to_vec(),
            handles: proto::Handles::Handles(vec![]),
        },
    );

    let FDomainEvent::WroteChannel(got_tid, Ok(())) = fdomain.next().await.unwrap() else {
        panic!()
    };
    assert_eq!(tid_1, got_tid);

    fdomain.read_channel_streaming_start(
        tid_2,
        proto::ChannelReadChannelStreamingStartRequest { handle: hid_b },
    );

    let FDomainEvent::ChannelStreamingReadStart(got_tid, Ok(())) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_2, got_tid);

    let FDomainEvent::ChannelStreamingData(proto::ChannelOnChannelStreamingDataRequest {
        handle: got_handle,
        channel_sent:
            proto::ChannelSent::Message(proto::ChannelMessage { data: got_data, handles: got_handles }),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(hid_b, got_handle);
    assert_eq!(data_a, got_data.as_slice());
    assert!(got_handles.is_empty());

    fdomain.write_channel(
        tid_3,
        proto::ChannelWriteChannelRequest {
            handle: hid_a,
            data: data_b.to_vec(),
            handles: proto::Handles::Handles(vec![]),
        },
    );

    let FDomainEvent::WroteChannel(got_tid, Ok(())) = fdomain.next().await.unwrap() else {
        panic!()
    };
    assert_eq!(tid_3, got_tid);

    let FDomainEvent::ChannelStreamingData(proto::ChannelOnChannelStreamingDataRequest {
        handle: got_handle,
        channel_sent:
            proto::ChannelSent::Message(proto::ChannelMessage { data: got_data, handles: got_handles }),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(hid_b, got_handle);
    assert_eq!(data_b, got_data.as_slice());
    assert!(got_handles.is_empty());
}

#[fuchsia::test]
async fn channel_async_read_detect_close() {
    let mut fdomain = FDomain::new_empty();
    let hid_a = 0;
    let hid_b = 2;

    assert!(fdomain
        .create_channel(proto::ChannelCreateChannelRequest {
            handles: [proto::NewHandleId { id: hid_a }, proto::NewHandleId { id: hid_b },],
        })
        .is_ok());

    let hid_a = proto::HandleId { id: hid_a };
    let hid_b = proto::HandleId { id: hid_b };

    let tid_1 = 1.try_into().unwrap();
    let tid_2 = 2.try_into().unwrap();
    let tid_3 = 3.try_into().unwrap();

    let data_a = b"The ghosts o' robot dinosaurs? Cor!";

    fdomain.write_channel(
        tid_1,
        proto::ChannelWriteChannelRequest {
            handle: hid_a,
            data: data_a.to_vec(),
            handles: proto::Handles::Handles(vec![]),
        },
    );

    let FDomainEvent::WroteChannel(got_tid, Ok(())) = fdomain.next().await.unwrap() else {
        panic!()
    };
    assert_eq!(tid_1, got_tid);

    fdomain.read_channel_streaming_start(
        tid_2,
        proto::ChannelReadChannelStreamingStartRequest { handle: hid_b },
    );

    let FDomainEvent::ChannelStreamingReadStart(got_tid, Ok(())) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_2, got_tid);

    let FDomainEvent::ChannelStreamingData(proto::ChannelOnChannelStreamingDataRequest {
        handle: got_handle,
        channel_sent:
            proto::ChannelSent::Message(proto::ChannelMessage { data: got_data, handles: got_handles }),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(hid_b, got_handle);
    assert_eq!(data_a, got_data.as_slice());
    assert!(got_handles.is_empty());

    fdomain.close(tid_3, proto::FDomainCloseRequest { handles: vec![hid_a] });

    let FDomainEvent::ClosedHandle(got_tid, Ok(())) = fdomain.next().await.unwrap() else {
        panic!()
    };
    assert_eq!(tid_3, got_tid);

    let FDomainEvent::ChannelStreamingData(proto::ChannelOnChannelStreamingDataRequest {
        handle: got_handle,
        channel_sent: proto::ChannelSent::Stopped(proto::AioStopped { error: Some(err) }),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(*err, proto::Error::TargetError(fidl::Status::PEER_CLOSED.into_raw()));
    assert_eq!(hid_b, got_handle);
}

#[fuchsia::test]
async fn channel_async_read_stop() {
    let mut fdomain = FDomain::new_empty();
    let hid_a = 0;
    let hid_b = 2;

    assert!(fdomain
        .create_channel(proto::ChannelCreateChannelRequest {
            handles: [proto::NewHandleId { id: hid_a }, proto::NewHandleId { id: hid_b },],
        })
        .is_ok());

    let hid_a = proto::HandleId { id: hid_a };
    let hid_b = proto::HandleId { id: hid_b };

    let tid_1 = 1.try_into().unwrap();
    let tid_2 = 2.try_into().unwrap();
    let tid_3 = 3.try_into().unwrap();
    let tid_4 = 4.try_into().unwrap();
    let tid_5 = 5.try_into().unwrap();

    let data_a = b"The ghosts o' robot dinosaurs? Cor!";
    let data_b = b"An educational robot vaudeville show about quantum mechanics";

    fdomain.write_channel(
        tid_1,
        proto::ChannelWriteChannelRequest {
            handle: hid_a,
            data: data_a.to_vec(),
            handles: proto::Handles::Handles(vec![]),
        },
    );

    let FDomainEvent::WroteChannel(got_tid, Ok(())) = fdomain.next().await.unwrap() else {
        panic!()
    };
    assert_eq!(tid_1, got_tid);

    fdomain.read_channel_streaming_start(
        tid_2,
        proto::ChannelReadChannelStreamingStartRequest { handle: hid_b },
    );

    let FDomainEvent::ChannelStreamingReadStart(got_tid, Ok(())) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_2, got_tid);

    let FDomainEvent::ChannelStreamingData(proto::ChannelOnChannelStreamingDataRequest {
        handle: got_handle,
        channel_sent:
            proto::ChannelSent::Message(proto::ChannelMessage { data: got_data, handles: got_handles }),
    }) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(hid_b, got_handle);
    assert_eq!(data_a, got_data.as_slice());
    assert!(got_handles.is_empty());

    fdomain.read_channel_streaming_stop(
        tid_3,
        proto::ChannelReadChannelStreamingStopRequest { handle: hid_b },
    );

    let FDomainEvent::ChannelStreamingReadStop(got_tid, Ok(())) = fdomain.next().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(tid_3, got_tid);

    fdomain.write_channel(
        tid_4,
        proto::ChannelWriteChannelRequest {
            handle: hid_a,
            data: data_b.to_vec(),
            handles: proto::Handles::Handles(vec![]),
        },
    );

    let FDomainEvent::WroteChannel(got_tid, Ok(())) = fdomain.next().await.unwrap() else {
        panic!()
    };
    assert_eq!(tid_4, got_tid);

    fdomain.read_channel(tid_5, proto::ChannelReadChannelRequest { handle: hid_b });

    let FDomainEvent::ChannelData(
        got_tid,
        Ok(proto::ChannelMessage { data: got_data, handles: got_handles }),
    ) = fdomain.next().await.unwrap()
    else {
        panic!()
    };

    assert_eq!(tid_5, got_tid);
    assert_eq!(data_b, got_data.as_slice());
    assert!(got_handles.is_empty())
}
