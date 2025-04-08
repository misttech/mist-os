// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async as _;

#[cfg(feature = "fdomain")]
pub use fdomain_client::*;

#[cfg(feature = "fdomain")]
pub type Dialect = fdomain_client::fidl::FDomainResourceDialect;

#[cfg(not(feature = "fdomain"))]
pub type Dialect = ::fidl::encoding::DefaultFuchsiaResourceDialect;

#[cfg(feature = "fdomain")]
pub use fdomain_client::Channel as AsyncChannel;

#[cfg(not(feature = "fdomain"))]
pub use ::fidl::endpoints::ProxyHasDomain;

#[cfg(feature = "fdomain")]
pub use fdomain_client::fidl::Proxy as ProxyHasDomain;

#[cfg(not(feature = "fdomain"))]
pub use ::fidl::*;

#[cfg(not(feature = "fdomain"))]
#[cfg(target_os = "fuchsia")]
pub use zx::MessageBuf;

#[cfg(not(feature = "fdomain"))]
#[cfg(not(target_os = "fuchsia"))]
pub use fuchsia_async::emulated_handle::MessageBuf;

#[cfg(not(feature = "fdomain"))]
pub mod fidl {
    pub use ::fidl::endpoints::*;
}
