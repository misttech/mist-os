// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::channel::HandleOp;
use crate::{AnyHandle, Client, Error, FDomainTransport, HandleBased};
use fdomain_container::wire::FDomainCodec;
use fdomain_container::FDomain;
use fidl_fuchsia_fdomain::Error as FDomainError;
use futures::stream::Stream;
use futures::{FutureExt, StreamExt};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

#[derive(Clone, Debug)]
struct TestError(String);

impl std::error::Error for TestError {}

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

struct TestFDomain(FDomainCodec, Arc<Mutex<FaultInjectorState>>);

impl TestFDomain {
    fn new_client() -> (Arc<Client>, FaultInjector) {
        let fault_injector =
            FaultInjector(Arc::new(Mutex::new(FaultInjectorState::SendBadMessages(Vec::new()))));
        let (client, fut) = Client::new(TestFDomain(
            FDomainCodec::new(FDomain::new_empty()),
            Arc::clone(&fault_injector.0),
        ));
        fuchsia_async::Task::spawn(fut).detach();
        (client, fault_injector)
    }
}

impl FDomainTransport for TestFDomain {
    fn poll_send_message(
        mut self: Pin<&mut Self>,
        msg: &[u8],
        _ctx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(self.0.message(msg).map_err(std::io::Error::other))
    }
}

impl Stream for TestFDomain {
    type Item = Result<Box<[u8]>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        {
            let mut faults = self.1.lock().unwrap();
            match &mut *faults {
                FaultInjectorState::Error(e) => {
                    return Poll::Ready(Some(Err(std::io::Error::other(e.clone()))));
                }

                FaultInjectorState::SendBadMessages(f) => {
                    if let Some(send) = f.pop() {
                        return Poll::Ready(Some(Ok(send)));
                    }
                }
            }
        }

        Pin::new(&mut self.0).poll_next(cx).map_err(std::io::Error::other)
    }
}

enum FaultInjectorState {
    Error(TestError),
    SendBadMessages(Vec<Box<[u8]>>),
}

struct FaultInjector(Arc<Mutex<FaultInjectorState>>);

impl FaultInjector {
    fn inject(&self, e: TestError) {
        *self.0.lock().unwrap() = FaultInjectorState::Error(e)
    }

    fn send_garbage(&self, g: impl AsRef<[u8]>) {
        let mut f = self.0.lock().unwrap();
        let FaultInjectorState::SendBadMessages(queue) = &mut *f else {
            panic!("Injected failure then still tried to send garbage!");
        };
        queue.insert(0, Box::from(g.as_ref()));
    }
}

#[fuchsia::test]
async fn socket() {
    let (client, _) = TestFDomain::new_client();

    let (a, b) = client.create_stream_socket();
    const TEST_STR: &[u8] = b"Feral Cats Move In Mysterious Ways";

    a.write_all(TEST_STR).await.unwrap();

    let mut got = Vec::with_capacity(TEST_STR.len());
    let mut buf = [0u8; TEST_STR.len()];

    while got.len() < TEST_STR.len() {
        let new_bytes = b.read(&mut buf).await.unwrap();
        got.extend_from_slice(&buf[..new_bytes]);
    }

    assert_eq!(TEST_STR, got.as_slice());
}

#[fuchsia::test]
async fn datagram_socket() {
    let (client, _) = TestFDomain::new_client();

    let (a, b) = client.create_datagram_socket();
    const TEST_STR_1: &[u8] = b"Feral Cats Move In Mysterious Ways";
    const TEST_STR_2: &[u8] = b"Joyous Throbbing! Jubilant Pulsing!";

    a.write_all(TEST_STR_1).await.unwrap();
    a.write_all(TEST_STR_2).await.unwrap();

    let mut buf = [0u8; 2];

    let new_bytes = b.read(&mut buf).await.unwrap();
    assert_eq!(new_bytes, 2);
    assert_eq!(&TEST_STR_1[..2], &buf);

    let new_bytes = b.read(&mut buf).await.unwrap();
    assert_eq!(new_bytes, 2);
    assert_eq!(&TEST_STR_2[..2], &buf);
}

#[fuchsia::test]
async fn datagram_socket_underflow() {
    let (client, _) = TestFDomain::new_client();

    let (a, b) = client.create_datagram_socket();
    const TEST_STR_1: &[u8] = b"Feral Cats Move In Mysterious Ways";
    const TEST_STR_2: &[u8] = b"Joyous Throbbing! Jubilant Pulsing!";

    a.write_all(TEST_STR_1).await.unwrap();
    a.write_all(TEST_STR_2).await.unwrap();

    const MAX_LEN: usize =
        if TEST_STR_1.len() > TEST_STR_2.len() { TEST_STR_1.len() } else { TEST_STR_2.len() };
    let mut buf = [0u8; MAX_LEN * 2];

    let new_bytes = b.read(&mut buf).await.unwrap();
    assert_eq!(new_bytes, TEST_STR_1.len());
    assert_eq!(TEST_STR_1, &buf[..TEST_STR_1.len()]);

    let new_bytes = b.read(&mut buf).await.unwrap();
    assert_eq!(new_bytes, TEST_STR_2.len());
    assert_eq!(TEST_STR_2, &buf[..TEST_STR_2.len()]);
}

#[fuchsia::test]
async fn channel() {
    let (client, _) = TestFDomain::new_client();

    let (a, b) = client.create_channel();
    let (c, d) = client.create_stream_socket();
    const TEST_STR_1: &[u8] = b"Feral Cats Move In Mysterious Ways";
    const TEST_STR_2: &[u8] = b"Joyous Throbbing! Jubilant Pulsing!";

    a.fdomain_write(TEST_STR_1, vec![c.into()]).await.unwrap();
    d.write_all(TEST_STR_2).await.unwrap();

    let mut msg = b.recv_msg().await.unwrap();

    assert_eq!(TEST_STR_1, msg.bytes.as_slice());

    let handle = msg.handles.pop().unwrap();
    assert!(msg.handles.is_empty());

    let expect_rights = fidl::Rights::DUPLICATE
        | fidl::Rights::TRANSFER
        | fidl::Rights::READ
        | fidl::Rights::WRITE
        | fidl::Rights::GET_PROPERTY
        | fidl::Rights::SET_PROPERTY
        | fidl::Rights::SIGNAL
        | fidl::Rights::SIGNAL_PEER
        | fidl::Rights::WAIT
        | fidl::Rights::INSPECT
        | fidl::Rights::MANAGE_SOCKET;
    assert_eq!(expect_rights, handle.rights);

    let AnyHandle::Socket(e) = handle.handle else { panic!() };

    let mut got = Vec::with_capacity(TEST_STR_2.len());
    let mut buf = [0u8; TEST_STR_2.len()];

    while got.len() < TEST_STR_2.len() {
        let new_bytes = e.read(&mut buf).await.unwrap();
        got.extend_from_slice(&buf[..new_bytes]);
    }

    assert_eq!(TEST_STR_2, got.as_slice());
}

#[fuchsia::test]
async fn socket_async() {
    let (client, fault_injector) = TestFDomain::new_client();

    let (a, b) = client.create_stream_socket();
    const TEST_STR_A: &[u8] = b"Feral Cats Move In Mysterious Ways";
    const TEST_STR_B: &[u8] = b"Almost all of our feelings were programmed in to us.";

    let (mut b_reader, b_writer) = b.stream().unwrap();
    b_writer.write_all(TEST_STR_A).await.unwrap();

    let write_side = async move {
        let mut got = Vec::with_capacity(TEST_STR_A.len());
        let mut buf = [0u8; TEST_STR_A.len()];

        while got.len() < TEST_STR_A.len() {
            let new_bytes = a.read(&mut buf).await.unwrap();
            got.extend_from_slice(&buf[..new_bytes]);
        }

        assert_eq!(TEST_STR_A, got.as_slice());

        for _ in 0..5 {
            a.write_all(TEST_STR_A).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
            a.write_all(TEST_STR_B).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
        }

        fault_injector.inject(TestError("Connection failed".to_owned()));

        let err = a.write_all(TEST_STR_A).await.unwrap_err();
        let Error::Transport(err) = err else { panic!("Wrong error type!") };

        let TestError(err) = err.get_ref().unwrap().downcast_ref().unwrap();
        assert_eq!("Connection failed", err);
    };

    let read_side = async move {
        let mut buf = Vec::new();
        buf.resize((TEST_STR_A.len() + TEST_STR_B.len()) * 5, 0);

        for mut buf in buf.chunks_mut(20) {
            while !buf.is_empty() {
                let len = b_reader.read(buf).await.unwrap();
                buf = &mut buf[len..];
            }
        }

        let err = b_reader.read(&mut [0]).await.unwrap_err();
        let Error::Transport(err) = err else { panic!("Wrong error type!") };

        let TestError(err) = err.get_ref().unwrap().downcast_ref().unwrap();
        assert_eq!("Connection failed", err);

        let mut buf = buf.as_mut_slice();

        for _ in 0..5 {
            assert!(buf.starts_with(TEST_STR_A));
            buf = &mut buf[TEST_STR_A.len()..];
            assert!(buf.starts_with(TEST_STR_B));
            buf = &mut buf[TEST_STR_B.len()..];
        }

        assert!(buf.is_empty());
    };

    futures::future::join(read_side, write_side).await;
}

#[fuchsia::test]
async fn socket_drop_read_fut() {
    let (client, _fault_injector) = TestFDomain::new_client();

    let mut buf1 = [0u8; 2];
    let mut buf2 = [0u8; 2];
    let mut buf3 = [0u8; 2];

    let (a, b) = client.create_stream_socket();
    let mut fut1 = b.read(&mut buf1);
    let mut fut2 = b.read(&mut buf2);
    let mut fut3 = b.read(&mut buf3);

    let mut null_cx = std::task::Context::from_waker(futures::task::noop_waker_ref());
    assert!(fut1.poll_unpin(&mut null_cx).is_pending());
    assert!(fut2.poll_unpin(&mut null_cx).is_pending());
    assert!(fut3.poll_unpin(&mut null_cx).is_pending());

    a.write_all(b"abcd").await.unwrap();

    assert_eq!(2, fut1.await.unwrap());
    std::mem::drop(fut2);
    assert_eq!(2, fut3.await.unwrap());

    assert_eq!(b"ab", &buf1);
    assert_eq!(b"cd", &buf3);
}

#[fuchsia::test]
async fn channel_async() {
    let (client, fault_injector) = TestFDomain::new_client();

    let (a, b) = client.create_channel();
    let test_str_a = b"Feral Cats Move In Mysterious Ways";
    let test_str_b = b"Almost all of our feelings were programmed in to us.";

    let (mut b_reader, b_writer) = b.stream().unwrap();
    b_writer.fdomain_write(test_str_a, Vec::new()).await.unwrap();

    let write_side = async move {
        let msg = a.recv_msg().await.unwrap();

        assert_eq!(test_str_a, msg.bytes.as_slice());
        assert!(msg.handles.is_empty());

        for _ in 0..5 {
            a.fdomain_write(test_str_a, Vec::new()).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
            a.fdomain_write(test_str_b, Vec::new()).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
        }

        fault_injector.inject(TestError("Connection failed".to_owned()));

        let err = a.fdomain_write(test_str_a, Vec::new()).await.unwrap_err();
        let Error::Transport(err) = err else { panic!("Wrong error type!") };

        let TestError(err) = err.get_ref().unwrap().downcast_ref().unwrap();
        assert_eq!("Connection failed", err);
    };

    let read_side = async move {
        let mut msgs = Vec::new();

        for _ in 0..10 {
            msgs.push(b_reader.next().await.unwrap().unwrap());
        }

        let err = b_reader.next().await.unwrap().unwrap_err();
        let Error::Transport(err) = err else { panic!("Wrong error type!") };

        let TestError(err) = err.get_ref().unwrap().downcast_ref().unwrap();
        assert_eq!("Connection failed", err);

        for pair in msgs.chunks(2) {
            let a = &pair[0];
            let b = &pair[1];
            assert_eq!(test_str_a, a.bytes.as_slice());
            assert_eq!(test_str_b, b.bytes.as_slice());
            assert!(a.handles.is_empty());
            assert!(b.handles.is_empty());
        }
    };

    futures::future::join(read_side, write_side).await;
}

#[fuchsia::test]
async fn channel_async_shutdown() {
    let (client, _fault_injector) = TestFDomain::new_client();

    let (a, b) = client.create_channel();
    let test_str_a = b"Feral Cats Move In Mysterious Ways";
    let test_str_b = b"Almost all of our feelings were programmed in to us.";

    let (mut b_reader, b_writer) = b.stream().unwrap();
    b_writer.fdomain_write(test_str_a, Vec::new()).await.unwrap();

    let write_side = async move {
        let msg = a.recv_msg().await.unwrap();

        assert_eq!(test_str_a, msg.bytes.as_slice());
        assert!(msg.handles.is_empty());

        for _ in 0..5 {
            a.fdomain_write(test_str_a, Vec::new()).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
            a.fdomain_write(test_str_b, Vec::new()).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
        }

        println!("Dropping writer");
        std::mem::drop(a);
    };

    let read_side = async move {
        let mut msgs = Vec::new();

        for _ in 0..10 {
            msgs.push(b_reader.next().await.unwrap().unwrap());
        }

        println!("Waiting for error");
        let err = b_reader.next().await.unwrap().unwrap_err();
        println!("Got error");
        let Error::FDomain(FDomainError::TargetError(err)) = err else {
            panic!("Wrong error type!")
        };

        assert_eq!(fidl::Status::PEER_CLOSED.into_raw(), err);

        for pair in msgs.chunks(2) {
            let a = &pair[0];
            let b = &pair[1];
            assert_eq!(test_str_a, a.bytes.as_slice());
            assert_eq!(test_str_b, b.bytes.as_slice());
            assert!(a.handles.is_empty());
            assert!(b.handles.is_empty());
        }
    };

    futures::future::join(read_side, write_side).await;
}

#[fuchsia::test]
async fn bad_tx() {
    let (client, fault_injector) = TestFDomain::new_client();

    let (a, b) = client.create_channel();
    let test_str_a = b"Feral Cats Move In Mysterious Ways";
    a.fdomain_write(test_str_a, Vec::new()).await.unwrap();
    fault_injector.send_garbage(b"*splot*");
    let err = b.recv_msg().await.unwrap_err();
    let err2 = b.recv_msg().await.unwrap_err();
    assert_eq!(err.to_string(), err2.to_string());
}

#[fuchsia::test]
async fn channel_read_stream_read() {
    let (client, _) = TestFDomain::new_client();

    let (a, b) = client.create_channel();

    let read_1 = b.recv_msg();
    let read_2 = b.recv_msg();

    let test_str_a = b"Feral Cats Move In Mysterious Ways";
    let test_str_b = b"Almost all of our feelings were programmed in to us.";
    let test_str_c = b"Joyous Throbbing! Jubilant Pulsing!";
    a.fdomain_write(test_str_a, Vec::new()).await.unwrap();
    a.fdomain_write(test_str_b, Vec::new()).await.unwrap();
    a.fdomain_write(test_str_c, Vec::new()).await.unwrap();
    a.fdomain_write(test_str_b, Vec::new()).await.unwrap();

    let (mut stream, writer) = b.stream().unwrap();
    let read_1 = read_1.await.unwrap();
    let read_2 = read_2.await.unwrap();

    assert_eq!(test_str_a, read_1.bytes.as_slice());
    assert_eq!(test_str_b, read_2.bytes.as_slice());

    let stream_read = stream.next().await.unwrap().unwrap();
    assert_eq!(test_str_c, stream_read.bytes.as_slice());

    let b = stream.rejoin(writer);
    let read_3 = b.recv_msg().await.unwrap();
    assert_eq!(test_str_b, read_3.bytes.as_slice());
}

#[fuchsia::test]
async fn channel_too_big() {
    let (client, _) = TestFDomain::new_client();

    let (a, b) = client.create_channel();

    // Test with fdomain_write
    let err = a
        .fdomain_write(&[0xABu8; zx_types::ZX_CHANNEL_MAX_MSG_BYTES as usize + 1], vec![])
        .await
        .unwrap_err();

    let Error::FDomain(FDomainError::TargetError(i)) = err else { panic!() };

    assert_eq!(fidl::Status::OUT_OF_RANGE.into_raw(), i);

    let mut too_many_handles =
        Vec::with_capacity(zx_types::ZX_CHANNEL_MAX_MSG_HANDLES as usize + 1);
    for _ in 0..(zx_types::ZX_CHANNEL_MAX_MSG_HANDLES + 1) {
        too_many_handles.push(client.create_event().into_handle());
    }

    let err = a.fdomain_write(b"", too_many_handles).await.unwrap_err();

    let Error::FDomain(FDomainError::TargetError(i)) = err else { panic!() };

    assert_eq!(fidl::Status::OUT_OF_RANGE.into_raw(), i);

    // Test with zircon-like write
    let err =
        a.write(&[0xABu8; zx_types::ZX_CHANNEL_MAX_MSG_BYTES as usize + 1], vec![]).unwrap_err();

    let Error::FDomain(FDomainError::TargetError(i)) = err else { panic!() };

    assert_eq!(fidl::Status::OUT_OF_RANGE.into_raw(), i);

    let mut too_many_handles =
        Vec::with_capacity(zx_types::ZX_CHANNEL_MAX_MSG_HANDLES as usize + 1);
    for _ in 0..(zx_types::ZX_CHANNEL_MAX_MSG_HANDLES + 1) {
        too_many_handles.push(client.create_event().into_handle());
    }

    let err = a.write(b"", too_many_handles).unwrap_err();

    let Error::FDomain(FDomainError::TargetError(i)) = err else { panic!() };

    assert_eq!(fidl::Status::OUT_OF_RANGE.into_raw(), i);

    // Test with fdomain_write_etc
    let err = a
        .fdomain_write_etc(&[0xABu8; zx_types::ZX_CHANNEL_MAX_MSG_BYTES as usize + 1], vec![])
        .await
        .unwrap_err();

    let Error::FDomain(FDomainError::TargetError(i)) = err else { panic!() };

    assert_eq!(fidl::Status::OUT_OF_RANGE.into_raw(), i);

    let mut too_many_handles =
        Vec::with_capacity(zx_types::ZX_CHANNEL_MAX_MSG_HANDLES as usize + 1);
    for _ in 0..(zx_types::ZX_CHANNEL_MAX_MSG_HANDLES + 1) {
        too_many_handles
            .push(HandleOp::Move(client.create_event().into_handle(), fidl::Rights::SAME_RIGHTS));
    }

    let err = a.fdomain_write_etc(b"", too_many_handles).await.unwrap_err();

    let Error::FDomain(FDomainError::TargetError(i)) = err else { panic!() };

    assert_eq!(fidl::Status::OUT_OF_RANGE.into_raw(), i);

    // Make sure channel still functions
    const TEST_STR_1: &[u8] = b"Feral Cats Move In Mysterious Ways";

    a.fdomain_write(TEST_STR_1, vec![]).await.unwrap();

    let msg = b.recv_msg().await.unwrap();

    assert_eq!(TEST_STR_1, msg.bytes.as_slice());
}
