// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::borrow::Borrow;
use std::panic::Location;

use fuchsia_async::scope::{ScopeActiveGuard, ScopeHandle};
use fuchsia_async::JoinHandle;
use futures::{Future, FutureExt as _};

#[derive(Debug)]
pub(crate) struct ScopeFinishedError;

pub(crate) trait ScopeExt: Borrow<ScopeHandle> {
    #[track_caller]
    fn spawn_guarded_assert_cancelled(
        &self,
        guard: ScopeActiveGuard,
        fut: impl Future<Output = ()> + Send + 'static,
    ) -> JoinHandle<()> {
        // Get location info now because we're not going to have it in the
        // scope.
        let location = Location::caller();
        self.borrow().spawn(fut.then(move |()| {
            assert!(
                guard.as_handle().is_cancelled(),
                "task from {location} exited before scope was cancelled"
            );
            futures::future::ready(())
        }))
    }

    #[track_caller]
    fn spawn_new_guard_assert_cancelled(
        &self,
        fut: impl Future<Output = ()> + Send + 'static,
    ) -> Result<JoinHandle<()>, ScopeFinishedError> {
        let guard = self.borrow().active_guard().ok_or(ScopeFinishedError)?;
        Ok(self.spawn_guarded_assert_cancelled(guard, fut))
    }
}

impl<O> ScopeExt for O where O: Borrow<ScopeHandle> {}
