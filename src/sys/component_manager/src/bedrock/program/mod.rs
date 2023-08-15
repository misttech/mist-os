// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use cm_runner::{Runner, StartInfo};
use fidl::endpoints;
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_diagnostics_types as fdiagnostics;
use fuchsia_async::Task;
use fuchsia_zircon as zx;
use futures::{channel::oneshot, future::BoxFuture};
use thiserror::Error;
use zx::{AsHandleRef, Koid};

mod component_controller;
use component_controller::ComponentController;

/// A [Program] is a unit of execution.
///
/// After a program starts running, it may optionally publish diagnostics,
/// and may eventually terminate.
pub struct Program {
    controller: ComponentController,

    // Only used by tests. Internally, it is always the KOID of the controller
    // server endpoint. But we are free to evolve that.
    #[allow(dead_code)]
    koid: Koid,

    // Only here to keep the diagnostics task alive. Never read.
    _send_diagnostics: Task<()>,
}

impl Program {
    /// Starts running a program using the `runner`.
    ///
    /// TODO(fxbug.dev/122024): Change `start_info` to a `Dict` and `DeliveryMap` as runners
    /// migrate to use sandboxes.
    ///
    /// TODO(fxbug.dev/122024): This API allows users to create orphaned programs that's not
    /// associated with anything else. Once we have a bedrock component concept, we might
    /// want to require a containing component to start a program.
    ///
    /// TODO(fxbug.dev/122024): Since diagnostic information is only available once,
    /// the framework should be the one that get it. That's another reason to limit this API.
    pub async fn start(
        runner: &dyn Runner,
        start_info: StartInfo,
        diagnostics_sender: oneshot::Sender<fdiagnostics::ComponentDiagnostics>,
    ) -> Program {
        let (controller, server_end) =
            endpoints::create_proxy::<fcrunner::ComponentControllerMarker>()
                .expect("creating FIDL proxy should not fail");
        let koid = server_end
            .as_handle_ref()
            .basic_info()
            .expect("basic info should not require any rights")
            .koid;

        runner.start(start_info, server_end).await;
        let mut controller = ComponentController::new(controller);
        let diagnostics_receiver = controller.take_diagnostics_receiver().unwrap();
        let send_diagnostics = Task::spawn(async move {
            let diagnostics = diagnostics_receiver.await;
            if let Ok(diagnostics) = diagnostics {
                _ = diagnostics_sender.send(diagnostics);
            }
        });
        Program { controller, koid, _send_diagnostics: send_diagnostics }
    }

    /// Request to stop the program.
    pub fn stop(&self) -> Result<(), Error> {
        self.controller.stop().map_err(Error::Internal)?;
        Ok(())
    }

    /// Request to stop this program immediately.
    pub fn kill(&self) -> Result<(), Error> {
        self.controller.kill().map_err(Error::Internal)?;
        Ok(())
    }

    /// Wait for the program to terminate, with an epitaph specified in the
    /// `fuchsia.component.runner/ComponentController` FIDL protocol documentation.
    pub fn on_terminate(&self) -> BoxFuture<'static, zx::Status> {
        self.controller.wait_for_epitaph()
    }

    /// Gets a [`Koid`] that will uniquely identify this program.
    #[cfg(test)]
    pub fn koid(&self) -> Koid {
        self.koid
    }

    /// Creates a program that does nothing but let us intercept requests to control its lifecycle.
    #[cfg(test)]
    pub fn mock_from_controller(
        controller: endpoints::ClientEnd<fcrunner::ComponentControllerMarker>,
    ) -> Program {
        let koid = controller
            .as_handle_ref()
            .basic_info()
            .expect("basic info should not require any rights")
            .related_koid;

        let controller = ComponentController::new(controller.into_proxy().unwrap());
        let send_diagnostics = Task::spawn(async {});
        Program { controller, koid, _send_diagnostics: send_diagnostics }
    }
}

#[derive(Error, Debug)]
pub enum Error {
    /// Internal errors are not meant to be meaningfully handled by the user.
    #[error("internal error: {0}")]
    Internal(fidl::Error),
}
