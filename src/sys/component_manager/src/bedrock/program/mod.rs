// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::runner::{builtin::RemoteRunner, Namespace, Runner};
use fidl::endpoints;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_data as fdata;
use fidl_fuchsia_diagnostics_types as fdiagnostics;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_mem as fmem;
use fidl_fuchsia_process as fprocess;
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
        runner: &RemoteRunner,
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

        runner.start(start_info.into(), server_end).await;
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

/// Information and capabilities used to start a program.
pub struct StartInfo {
    /// The resolved URL of the component.
    ///
    /// This is the canonical URL obtained by the component resolver after
    /// following redirects and resolving relative paths.
    pub resolved_url: String,

    /// The component's program declaration.
    /// This information originates from `ComponentDecl.program`.
    pub program: fdata::Dictionary,

    /// The namespace to provide to the component instance.
    ///
    /// A namespace specifies the set of directories that a component instance
    /// receives at start-up. Through the namespace directories, a component
    /// may access capabilities available to it. The contents of the namespace
    /// are mainly determined by the component's `use` declarations but may
    /// also contain additional capabilities automatically provided by the
    /// framework.
    ///
    /// By convention, a component's namespace typically contains some or all
    /// of the following directories:
    ///
    /// - "/svc": A directory containing services that the component requested
    ///           to use via its "import" declarations.
    /// - "/pkg": A directory containing the component's package, including its
    ///           binaries, libraries, and other assets.
    ///
    /// The mount points specified in each entry must be unique and
    /// non-overlapping. For example, [{"/foo", ..}, {"/foo/bar", ..}] is
    /// invalid.
    pub namespace: Namespace,

    /// The directory this component serves.
    pub outgoing_dir: Option<ServerEnd<fio::DirectoryMarker>>,

    /// The directory served by the runner to present runtime information about
    /// the component. The runner must either serve it, or drop it to avoid
    /// blocking any consumers indefinitely.
    pub runtime_dir: Option<ServerEnd<fio::DirectoryMarker>>,

    /// The numbered handles that were passed to the component.
    ///
    /// If the component does not support numbered handles, the runner is expected
    /// to close the handles.
    pub numbered_handles: Vec<fprocess::HandleInfo>,

    /// Binary representation of the component's configuration.
    ///
    /// # Layout
    ///
    /// The first 2 bytes of the data should be interpreted as an unsigned 16-bit
    /// little-endian integer which denotes the number of bytes following it that
    /// contain the configuration checksum. After the checksum, all the remaining
    /// bytes are a persistent FIDL message of a top-level struct. The struct's
    /// fields match the configuration fields of the component's compiled manifest
    /// in the same order.
    pub encoded_config: Option<fmem::Data>,

    /// An eventpair that debuggers can use to defer the launch of the component.
    ///
    /// For example, ELF runners hold off from creating processes in the component
    /// until ZX_EVENTPAIR_PEER_CLOSED is signaled on this eventpair. They also
    /// ensure that runtime_dir is served before waiting on this eventpair.
    /// ELF debuggers can query the runtime_dir to decide whether to attach before
    /// they drop the other side of the eventpair, which is sent in the payload of
    /// the DebugStarted event in fuchsia.component.events.
    pub break_on_start: Option<zx::EventPair>,
}

impl From<StartInfo> for fcrunner::ComponentStartInfo {
    fn from(start_info: StartInfo) -> Self {
        Self {
            resolved_url: Some(start_info.resolved_url),
            program: Some(start_info.program),
            ns: Some(start_info.namespace.into()),
            outgoing_dir: start_info.outgoing_dir,
            runtime_dir: start_info.runtime_dir,
            numbered_handles: Some(start_info.numbered_handles),
            encoded_config: start_info.encoded_config,
            break_on_start: start_info.break_on_start,
            ..Default::default()
        }
    }
}
