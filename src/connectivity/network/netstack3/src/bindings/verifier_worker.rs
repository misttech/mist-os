// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::time::Duration;

use fidl_fuchsia_update_verify::{
    NetstackVerifierRequest, NetstackVerifierRequestStream, VerifyOptions,
};

use futures::TryStreamExt as _;

pub(crate) async fn serve(requests: NetstackVerifierRequestStream) -> Result<(), fidl::Error> {
    requests
        .try_for_each(|request: NetstackVerifierRequest| async {
            const WAIT_DURATION: Duration = Duration::from_secs(15);

            let NetstackVerifierRequest::Verify { options, responder } = request;
            let VerifyOptions { .. } = options;

            // Wait an arbitrary amount of time; if we didn't crash, we're probably healthy.
            fuchsia_async::Timer::new(WAIT_DURATION).await;

            responder.send(Ok(()))
        })
        .await
}
