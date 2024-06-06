// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::WeakComponentToken,
    fidl::handle::{EventPair, Signals},
    fuchsia_async as fasync,
    fuchsia_zircon::Koid,
};

impl crate::RemotableCapability for WeakComponentToken {}

impl WeakComponentToken {
    async fn serve(server: EventPair) {
        fasync::OnSignals::new(&server, Signals::OBJECT_PEER_CLOSED).await.ok();
    }

    pub fn register(self, koid: Koid, server: EventPair) {
        crate::fidl::registry::insert(self.into(), koid, WeakComponentToken::serve(server));
    }
}
