// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod builtin;
pub mod component_controller;

use {
    async_trait::async_trait, fidl::endpoints::ServerEnd, fidl_fuchsia_component_runner as fcrunner,
};

/// Executes a component instance.
/// TODO: The runner should return a trait object to allow the component instance to be stopped,
/// binding to services, and observing abnormal termination.  In other words, a wrapper that
/// encapsulates fcrunner::ComponentController FIDL interfacing concerns.
/// TODO: Consider defining an internal representation for `fcrunner::ComponentStartInfo` so as to
/// further isolate the `Model` from FIDL interfacting concerns.
#[async_trait]
pub trait Runner: Sync + Send {
    #[must_use]
    async fn start(
        &self,
        start_info: fcrunner::ComponentStartInfo,
        server_end: ServerEnd<fcrunner::ComponentControllerMarker>,
    );
}
