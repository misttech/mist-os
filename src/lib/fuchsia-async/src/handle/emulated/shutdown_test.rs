// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fuchsia_async::emulated_handle::{shut_down_handles, Channel, MessageBuf},
    fuchsia_zircon_status::Status,
    std::{
        future::Future,
        pin::pin,
        task::{Context, Poll},
    },
};

fn main() {
    let mut noop_ctx = Context::from_waker(futures::task::noop_waker_ref());
    let (a, b) = Channel::create();

    a.write(&[1, 2, 3], &mut []).unwrap();
    std::mem::drop(a);

    let mut fut = pin!(shut_down_handles());

    assert_eq!(fut.as_mut().poll(&mut noop_ctx), Poll::Pending);

    let mut buf = MessageBuf::new();
    b.read(&mut buf).unwrap();
    assert_eq!(buf.bytes(), &[1, 2, 3]);
    assert_eq!(buf.n_handles(), 0);
    assert_eq!(b.read(&mut buf), Err(Status::PEER_CLOSED));

    let (c, _d) = Channel::create();
    assert_eq!(c.write(&[4, 5, 6], &mut []), Err(Status::SHOULD_WAIT));

    assert_eq!(fut.as_mut().poll(&mut noop_ctx), Poll::Ready(()));
}
