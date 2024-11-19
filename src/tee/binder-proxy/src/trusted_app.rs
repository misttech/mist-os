// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::{Arc, LazyLock};

use android_system_microfuchsia_trusted_app::aidl::android::system::microfuchsia::trusted_app::ITrustedApp::{ITrustedApp, BnTrustedApp};
use binder::{self, SpIBinder};
use fuchsia_component::client::connect_to_protocol_sync_at;

pub struct TrustedApp {
    _fidl_ta: LazyLock<
        Arc<fidl_fuchsia_tee::ApplicationSynchronousProxy>,
        Box<dyn FnOnce() -> Arc<fidl_fuchsia_tee::ApplicationSynchronousProxy> + Send>,
    >,
}

impl binder::Interface for TrustedApp {}

impl ITrustedApp for TrustedApp {
    fn openSession(&self) -> Result<(), binder::Status> {
        Ok(())
    }
}

impl TrustedApp {
    fn new(uuid: &str) -> Self {
        let uuid = uuid.to_string();
        let fidl_ta = LazyLock::<_, Box<dyn FnOnce() -> Arc<_> + Send>>::new(Box::new(move || {
            let ta_path = "/ta/".to_owned() + &uuid;
            let connection =
                connect_to_protocol_sync_at::<fidl_fuchsia_tee::ApplicationMarker>(ta_path)
                    .expect("Connecting to TA {uuid}");
            Arc::new(connection)
        }));
        Self { _fidl_ta: fidl_ta }
    }

    pub fn new_binder(uuid: &str) -> SpIBinder {
        let trusted_app = TrustedApp::new(uuid);
        BnTrustedApp::new_binder(trusted_app, binder::BinderFeatures::default()).as_binder()
    }
}
