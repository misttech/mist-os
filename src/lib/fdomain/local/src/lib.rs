// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fdomain_client::{Client, FDomainTransport};
use fdomain_container::wire::FDomainCodec;
use fdomain_container::FDomain;
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_io as fio;
use futures::stream::Stream;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// An FDomain that is designed to be used in the same process it was created
/// in. I.e. no networking, just a bucket of handles right here where you can
/// use them.
struct LocalFDomain(FDomainCodec);

impl LocalFDomain {
    /// Create a new FDomain client that points to a new local FDomain.
    fn new_client(
        namespace: impl Fn() -> Result<ClientEnd<fio::DirectoryMarker>, fidl::Status> + Send + 'static,
    ) -> Arc<Client> {
        let (client, fut) = Client::new(LocalFDomain(FDomainCodec::new(FDomain::new(namespace))));
        fuchsia_async::Task::spawn(fut).detach();
        client
    }
}

impl FDomainTransport for LocalFDomain {
    fn poll_send_message(
        mut self: Pin<&mut Self>,
        msg: &[u8],
        _ctx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(self.0.message(msg).map_err(std::io::Error::other))
    }
}

impl Stream for LocalFDomain {
    type Item = Result<Box<[u8]>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.0).poll_next(cx).map_err(std::io::Error::other)
    }
}

/// Create a new FDomain client that points to a new local FDomain.
pub fn local_client(
    namespace: impl Fn() -> Result<ClientEnd<fio::DirectoryMarker>, fidl::Status> + Send + 'static,
) -> Arc<Client> {
    LocalFDomain::new_client(namespace)
}
