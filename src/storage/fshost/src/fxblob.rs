// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {futures::StreamExt, std::sync::Arc, vfs::service};

/// Make a new vfs service node that implements fuchsia.update.verify.BlobfsVerifier
pub fn blobfs_verifier_service() -> Arc<service::Service> {
    service::host(
        move |mut stream: fidl_fuchsia_update_verify::BlobfsVerifierRequestStream| async move {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(fidl_fuchsia_update_verify::BlobfsVerifierRequest::Verify {
                        responder,
                        ..
                    }) => {
                        // TODO(fxbug.dev/126334): Implement by calling out to Fxfs' blob volume.
                        responder.send(&mut Ok(())).unwrap_or_else(|e| {
                            tracing::error!("failed to send Verify response. error: {:?}", e);
                        });
                    }
                    Err(e) => {
                        tracing::error!("BlobfsVerifier server failed: {:?}", e);
                        return;
                    }
                }
            }
        },
    )
}
