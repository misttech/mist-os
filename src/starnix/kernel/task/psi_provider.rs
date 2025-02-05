// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_component::client::connect_to_protocol_sync;
use std::sync::{Arc, OnceLock};

#[derive(Default)]
pub struct PsiProvider(
    OnceLock<Option<Arc<fidl_fuchsia_starnix_psi::PsiProviderSynchronousProxy>>>,
);

impl PsiProvider {
    pub fn get(&self) -> Option<&Arc<fidl_fuchsia_starnix_psi::PsiProviderSynchronousProxy>> {
        self.0
            .get_or_init(|| {
                let proxy =
                    connect_to_protocol_sync::<fidl_fuchsia_starnix_psi::PsiProviderMarker>()
                        .ok()?;
                // Our manifest lists PsiProvider as an optional protocol. Let's check if it's
                // connected for real or not.
                let _ = proxy.get_memory_pressure_stats(zx::Instant::INFINITE).ok()?;
                Some(Arc::new(proxy))
            })
            .as_ref()
    }
}
