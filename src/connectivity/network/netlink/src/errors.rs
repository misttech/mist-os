// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for shared errors between routes and interfaces watcher workers.

#[derive(Debug, thiserror::Error)]
pub(crate) enum EventLoopError<F: std::fmt::Debug, N: std::fmt::Debug> {
    /// Errors at the FIDL layer.
    ///
    /// Such as: cannot connect to protocol or watcher, loaded FIDL error
    /// from stream.
    #[error("fidl error: {0:?}")]
    Fidl(F),
    /// Errors at the Netstack layer.
    ///
    /// Such as: watcher event stream ended, or a struct from
    /// Netstack failed conversion.
    #[error("netstack error: {0:?}")]
    Netstack(N),
}
