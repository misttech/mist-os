// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;

use crate::header::HeaderSet;
use crate::operation::ResponseCode;

pub type ObexResult = Result<HeaderSet, (ResponseCode, HeaderSet)>;

/// An interface that implements the OBEX Server role.
/// This interface roughly corresponds to the operations defined in OBEX v1.5.
#[async_trait]
pub trait ObexServerHandler {
    /// A request to initiate the CONNECT operation.
    /// `headers` are the informational headers provided by the remote OBEX client.
    /// Returns `Ok` with any response headers if the CONNECT request is accepted.
    /// Returns `Err` with a rejection code and headers if the CONNECT request is rejected.
    async fn connect(&mut self, headers: HeaderSet) -> ObexResult;

    // TODO(fxbug.dev/125307): Add other operation types.
}

#[cfg(test)]
pub(crate) mod test_utils {
    use super::*;

    use parking_lot::Mutex;
    use std::sync::Arc;

    #[derive(Clone)]
    pub(crate) struct TestApplicationProfile {
        response: Arc<Mutex<Option<ObexResult>>>,
    }

    impl TestApplicationProfile {
        pub fn new() -> Self {
            Self { response: Arc::new(Mutex::new(None)) }
        }
        pub fn set_response(&self, response: ObexResult) {
            *self.response.lock() = Some(response);
        }
    }

    #[async_trait]
    impl ObexServerHandler for TestApplicationProfile {
        async fn connect(&mut self, _headers: HeaderSet) -> ObexResult {
            // Defaults to rejecting with `MethodNotAllowed`.
            self.response
                .lock()
                .take()
                .unwrap_or(Err((ResponseCode::MethodNotAllowed, HeaderSet::new())))
        }
    }
}
