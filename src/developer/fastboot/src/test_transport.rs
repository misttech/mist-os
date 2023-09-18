// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::reply::Reply;
use futures::task::{Context, Poll};
use futures::{AsyncRead, AsyncWrite};
use std::pin::Pin;

pub struct TestTransport {
    replies: Vec<Reply>,
}

impl AsyncRead for TestTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.replies.pop() {
            Some(r) => {
                let reply = Vec::<u8>::from(r);
                buf[..reply.len()].copy_from_slice(&reply);
                Poll::Ready(Ok(reply.len()))
            }
            None => Poll::Ready(Ok(0)),
        }
    }
}

impl AsyncWrite for TestTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        unimplemented!();
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        unimplemented!();
    }
}

impl TestTransport {
    pub fn new() -> Self {
        TestTransport { replies: Vec::new() }
    }

    pub fn push(&mut self, reply: Reply) {
        self.replies.push(reply);
    }
}
