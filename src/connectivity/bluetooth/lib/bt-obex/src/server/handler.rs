// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;

use crate::header::HeaderSet;
use crate::operation::ResponseCode;

/// An operation can be rejected with a `ResponseCode` and optional headers describing the
/// reason for rejection.
pub type ObexOperationError = (ResponseCode, HeaderSet);

pub type ObexResult = Result<HeaderSet, ObexOperationError>;

/// An interface that implements the OBEX Server role.
/// This interface roughly corresponds to the operations defined in OBEX v1.5.
#[async_trait]
pub trait ObexServerHandler {
    /// A request to initiate the CONNECT operation.
    /// `headers` are the informational headers provided by the remote OBEX client.
    /// Returns `Ok` with any response headers if the CONNECT request is accepted.
    /// Returns `Err` with a rejection code and headers if the CONNECT request is rejected.
    async fn connect(&mut self, headers: HeaderSet) -> ObexResult;

    /// A request to disconnect the OBEX connection.
    /// `headers` are the informational headers provided by the remote OBEX client.
    /// Returns informational headers in response to the request.
    async fn disconnect(&mut self, headers: HeaderSet) -> HeaderSet;

    /// A request to set the current working folder on the device.
    /// `headers` are the informational headers provided by the remote OBEX client.
    /// If `backup` is `true`, then the remote requests to backup one directory before setting the
    /// path.
    /// If `create` is `true`, then the remote requests to create the path if it does not exist.
    /// If `create` is `false` and the path doesn't exist, `Err` should be returned.
    /// Returns `Ok` with any response headers if the SET_PATH request is accepted.
    /// Returns `Err` with a rejection code and headers if the SET_PATH request is rejected.
    async fn set_path(&mut self, headers: HeaderSet, backup: bool, create: bool) -> ObexResult;

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

        async fn disconnect(&mut self, _headers: HeaderSet) -> HeaderSet {
            // Disconnect cannot be rejected so just take the response headers if they exist or
            // default to returning an empty HeaderSet.
            match self.response.lock().take() {
                Some(Ok(headers)) => headers,
                _ => HeaderSet::new(),
            }
        }

        async fn set_path(
            &mut self,
            _headers: HeaderSet,
            _backup: bool,
            _create: bool,
        ) -> ObexResult {
            // Defaults to rejecting with `Forbidden`.
            self.response.lock().take().unwrap_or(Err((ResponseCode::Forbidden, HeaderSet::new())))
        }
    }
}
