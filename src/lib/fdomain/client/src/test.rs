// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{AnyHandle, Client, Error, FDomainTransport};
use fdomain_container::wire::FDomainCodec;
use fdomain_container::FDomain;
use futures::stream::Stream;
use futures::StreamExt;
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
    let test_str = b"Feral Cats Move In Mysterious Ways";

    a.write_all(test_str).await.unwrap();

    let mut got = Vec::with_capacity(test_str.len());

    while got.len() < test_str.len() {
        got.append(&mut b.read(test_str.len() - got.len()).await.unwrap());
    }

    assert_eq!(test_str, got.as_slice());
}

#[fuchsia::test]
async fn channel() {
    let (client, _) = TestFDomain::new_client();

    let (a, b) = client.create_channel();
    let (c, d) = client.create_stream_socket();
    let test_str_1 = b"Feral Cats Move In Mysterious Ways";
    let test_str_2 = b"Joyous Throbbing! Jubilant Pulsing!";

    a.write(test_str_1, vec![c.into()]).await.unwrap();
    d.write_all(test_str_2).await.unwrap();

    let mut msg = b.recv_msg().await.unwrap();

    assert_eq!(test_str_1, msg.bytes.as_slice());

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

    let mut got = Vec::with_capacity(test_str_2.len());

    while got.len() < test_str_2.len() {
        got.append(&mut e.read(test_str_2.len() - got.len()).await.unwrap());
    }

    assert_eq!(test_str_2, got.as_slice());
}

#[fuchsia::test]
async fn socket_async() {
    let (client, fault_injector) = TestFDomain::new_client();

    let (a, b) = client.create_stream_socket();
    let test_str_a = b"Feral Cats Move In Mysterious Ways";
    let test_str_b = b"Almost all of our feelings were programmed in to us.";

    let (mut b_reader, b_writer) = b.stream().unwrap();
    b_writer.write_all(test_str_a).await.unwrap();

    let write_side = async move {
        let mut got = Vec::with_capacity(test_str_a.len());

        while got.len() < test_str_a.len() {
            got.append(&mut a.read(test_str_a.len() - got.len()).await.unwrap());
        }

        assert_eq!(test_str_a, got.as_slice());

        for _ in 0..5 {
            a.write_all(test_str_a).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
            a.write_all(test_str_b).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
        }

        fault_injector.inject(TestError("Connection failed".to_owned()));

        let err = a.write_all(test_str_a).await.unwrap_err();
        let Error::Transport(err) = err else { panic!("Wrong error type!") };

        let TestError(err) = err.get_ref().unwrap().downcast_ref().unwrap();
        assert_eq!("Connection failed", err);
    };

    let read_side = async move {
        let mut buf = Vec::new();
        buf.resize((test_str_a.len() + test_str_b.len()) * 5, 0);

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
            assert!(buf.starts_with(test_str_a));
            buf = &mut buf[test_str_a.len()..];
            assert!(buf.starts_with(test_str_b));
            buf = &mut buf[test_str_b.len()..];
        }

        assert!(buf.is_empty());
    };

    futures::future::join(read_side, write_side).await;
}

#[fuchsia::test]
async fn channel_async() {
    let (client, fault_injector) = TestFDomain::new_client();

    let (a, b) = client.create_channel();
    let test_str_a = b"Feral Cats Move In Mysterious Ways";
    let test_str_b = b"Almost all of our feelings were programmed in to us.";

    let (mut b_reader, b_writer) = b.stream().unwrap();
    b_writer.write(test_str_a, Vec::new()).await.unwrap();

    let write_side = async move {
        let msg = a.recv_msg().await.unwrap();

        assert_eq!(test_str_a, msg.bytes.as_slice());
        assert!(msg.handles.is_empty());

        for _ in 0..5 {
            a.write(test_str_a, Vec::new()).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
            a.write(test_str_b, Vec::new()).await.unwrap();
            fuchsia_async::Timer::new(std::time::Duration::from_millis(10)).await;
        }

        fault_injector.inject(TestError("Connection failed".to_owned()));

        let err = a.write(test_str_a, Vec::new()).await.unwrap_err();
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

// TODO: We can re-enable this test in a bit. An upcoming CL reworks a bunch of
// our internals and then this starts working again.
#[fuchsia::test]
#[ignore]
async fn bad_tx() {
    let (client, fault_injector) = TestFDomain::new_client();

    let (a, b) = client.create_channel();
    let test_str_a = b"Feral Cats Move In Mysterious Ways";
    a.write(test_str_a, Vec::new()).await.unwrap();
    fault_injector.send_garbage(b"*splot*");
    let err = b.recv_msg().await.unwrap_err();
    let err2 = b.recv_msg().await.unwrap_err();
    assert_eq!(err.to_string(), err2.to_string());
}
