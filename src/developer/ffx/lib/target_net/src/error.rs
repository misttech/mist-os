// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_posix as fposix;
use thiserror::Error;

/// Errors emitted by the `ffx_target_net` crate.
#[derive(Error, Debug)]
pub enum Error {
    #[error("unexpected FIDL error: {0}")]
    Fidl(#[from] fidl::Error),
    #[error("missing field in fidl response: {0}")]
    MissingField(&'static str),
    #[error("socket hung up")]
    Hangup,
    #[error("unexpected error clearing signals: {0}")]
    ClearingSignal(fidl::Status),
    #[error("unexpected error waiting on signals: {0}")]
    WaitingSignal(fidl::Status),
    #[error("could not open protocol: {0}")]
    OpenProtocol(#[source] anyhow::Error),
    #[error("create socket error: {0:?}")]
    CreateSocket(fposix::Errno),
    #[error("accept error: {0:?}")]
    Accept(fposix::Errno),
    #[error("connect error: {0:?}")]
    Connect(fposix::Errno),
    #[error("bind error: {0:?}")]
    Bind(fposix::Errno),
    #[error("close error: {0:?}")]
    Close(fidl::Status),
    #[error("listen error: {0:?}")]
    Listen(fposix::Errno),
    #[error("getsockname error: {0:?}")]
    GetSockName(fposix::Errno),
}
