// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    model::{
        error::{StartError, StopError},
        escrow::EscrowedState,
        token::InstanceToken,
    },
    runner::RemoteRunner,
};
use fidl::endpoints;
use fidl::endpoints::{ClientEnd, ServerEnd};
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_data as fdata;
use fidl_fuchsia_diagnostics_types as fdiagnostics;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_mem as fmem;
use fidl_fuchsia_process as fprocess;
use fuchsia_zircon as zx;
use futures::{
    channel::oneshot,
    future::{BoxFuture, Either},
    FutureExt,
};
use serve_processargs::NamespaceBuilder;
use vfs::execution_scope::ExecutionScope;

mod component_controller;
use component_controller::ComponentController;

/// A [Program] is a unit of execution.
///
/// After a program starts running, it may optionally publish diagnostics,
/// and may eventually terminate.
pub struct Program {
    controller: ComponentController,

    /// The directory that presents runtime information about the component. The
    /// runner must either serve the server endpoint, or drop it to avoid
    /// blocking any consumers indefinitely.
    runtime_dir: fio::DirectoryProxy,

    /// The scope in which the namespace is run. component_manager will use this to
    /// gracefully shutdown the namespace when the component is stopped.
    namespace_scope: ExecutionScope,
}

impl Program {
    /// Starts running a program using the `runner`.
    ///
    /// After successfully starting the program, it will serve the namespace given to the
    /// program. If [Program] is dropped or [Program::kill] is called, the namespace will no
    /// longer be served.
    ///
    /// TODO(https://fxbug.dev/42073001): Change `start_info` to a `Dict` and `DeliveryMap` as runners
    /// migrate to use sandboxes.
    ///
    /// TODO(https://fxbug.dev/42073001): This API allows users to create orphaned programs that's not
    /// associated with anything else. Once we have a bedrock component concept, we might
    /// want to require a containing component to start a program.
    ///
    /// TODO(https://fxbug.dev/42073001): Since diagnostic information is only available once,
    /// the framework should be the one that get it. That's another reason to limit this API.
    pub fn start(
        runner: &RemoteRunner,
        start_info: StartInfo,
        escrowed_state: EscrowedState,
        diagnostics_sender: oneshot::Sender<fdiagnostics::ComponentDiagnostics>,
        namespace_scope: ExecutionScope,
    ) -> Result<Program, StartError> {
        let (controller, server_end) =
            endpoints::create_proxy::<fcrunner::ComponentControllerMarker>().unwrap();
        let (runtime_dir, runtime_server) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let start_info = start_info.into_fidl(escrowed_state.outgoing_dir, runtime_server)?;

        runner.start(start_info, server_end);
        let controller = ComponentController::new(controller, Some(diagnostics_sender));
        Ok(Program { controller, runtime_dir, namespace_scope })
    }

    /// Gets the runtime directory of the program.
    pub fn runtime(&self) -> &fio::DirectoryProxy {
        &self.runtime_dir
    }

    /// Request to stop the program.
    pub fn stop(&self) -> Result<(), StopError> {
        self.controller.stop().map_err(StopError::Internal)?;
        Ok(())
    }

    /// Request to stop this program immediately.
    pub fn kill(&self) -> Result<(), StopError> {
        self.controller.kill().map_err(StopError::Internal)?;
        Ok(())
    }

    /// Wait for the program to terminate, with an epitaph specified in the
    /// `fuchsia.component.runner/ComponentController` FIDL protocol documentation.
    pub fn on_terminate(&self) -> BoxFuture<'static, zx::Status> {
        self.controller.wait_for_epitaph()
    }

    /// Stops or kills the program or returns early if either operation times out.
    ///
    /// If the program does not stop within the timeout, i.e. if `stop_timer` returns a value
    /// before the program stops, then the program is killed.
    ///
    /// Returns an error if the program could not be killed within the `kill_timer` timeout.
    pub async fn stop_or_kill_with_timeout<'a, 'b>(
        &self,
        stop_timer: BoxFuture<'a, ()>,
        kill_timer: BoxFuture<'b, ()>,
    ) -> Result<ComponentStopOutcome, StopError> {
        let outcome = match self.stop_with_timeout(stop_timer).await {
            Some(r) => r,
            None => {
                // We must have hit the stop timeout because calling stop didn't return
                // a result, move to killing the component.
                self.kill_with_timeout(kill_timer).await
            }
        }?;
        self.namespace_scope.shutdown();
        self.namespace_scope.wait().await;
        Ok(outcome)
    }

    /// Stops the program or returns early if the operation times out.
    ///
    /// Returns None on timeout, when `stop_timer` returns a value before the program terminates.
    pub async fn stop_with_timeout<'a>(
        &self,
        stop_timer: BoxFuture<'a, ()>,
    ) -> Option<Result<ComponentStopOutcome, StopError>> {
        // Ask the controller to stop the component
        if let Err(err) = self.stop() {
            return Some(Err(err));
        }

        // Wait for the controller to close the channel
        let channel_close = self.on_terminate().boxed();

        // Wait for either the timer to fire or the channel to close
        match futures::future::select(stop_timer, channel_close).await {
            Either::Left(((), _channel_close)) => None,
            Either::Right((_timer, _close_result)) => Some(Ok(ComponentStopOutcome {
                request: StopRequestSuccess::Stopped,
                component_exit_status: self.on_terminate().await,
            })),
        }
    }

    /// Kills the program or returns early if the operation times out.
    ///
    /// Returns None on timeout, when `kill_timer` returns a value before the program terminates.
    pub async fn kill_with_timeout<'a>(
        &self,
        kill_timer: BoxFuture<'a, ()>,
    ) -> Result<ComponentStopOutcome, StopError> {
        self.kill()?;

        // Wait for the controller to close the channel
        let channel_close = self.on_terminate().boxed();

        // If the control channel closes first, report the component to be
        // kill "normally", otherwise report it as killed after timeout.
        match futures::future::select(kill_timer, channel_close).await {
            Either::Left(((), _channel_close)) => Ok(ComponentStopOutcome {
                request: StopRequestSuccess::KilledAfterTimeout,
                component_exit_status: zx::Status::TIMED_OUT,
            }),
            Either::Right((_timer, _close_result)) => Ok(ComponentStopOutcome {
                request: StopRequestSuccess::Killed,
                component_exit_status: self.on_terminate().await,
            }),
        }
    }

    /// Drops the program and returns state that the program has escrowed, if any.
    pub fn finalize(self) -> Option<EscrowRequest> {
        let escrowed_state = self.controller.escrow().lock().take();
        drop(self);
        escrowed_state
    }

    /// Gets a [`Koid`] that will uniquely identify this program.
    #[cfg(test)]
    pub fn koid(&self) -> zx::Koid {
        self.controller.peer_koid()
    }

    /// Creates a program that does nothing but let us intercept requests to control its lifecycle.
    #[cfg(test)]
    pub fn mock_from_controller(
        controller: endpoints::ClientEnd<fcrunner::ComponentControllerMarker>,
    ) -> Program {
        let controller = ComponentController::new(controller.into_proxy().unwrap(), None);
        let (runtime_dir, _runtime_server) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        Program { controller, runtime_dir, namespace_scope: ExecutionScope::new() }
    }
}

#[derive(Debug, PartialEq)]
/// Represents the result of a request to stop a component, which has two
/// pieces. There is what happened to sending the request over the FIDL
/// channel to the controller and then what the exit status of the component
/// is. For example, the component might have exited with error before the
/// request was sent, in which case we encountered no error processing the stop
/// request and the component is considered to have terminated abnormally.
pub struct ComponentStopOutcome {
    /// The result of the request to stop the component.
    pub request: StopRequestSuccess,
    /// The final status of the component.
    pub component_exit_status: zx::Status,
}

#[derive(Debug, PartialEq)]
/// Outcomes of the stop request that are considered success. A request success
/// indicates that the request was sent without error over the
/// ComponentController channel or that sending the request was not necessary
/// because the component stopped previously.
pub enum StopRequestSuccess {
    /// The component did not stop in time, but was killed before the kill
    /// timeout.
    Killed,
    /// The component did not stop in time and was killed after the kill
    /// timeout was reached.
    KilledAfterTimeout,
    /// The component had no Controller, no request was sent, and therefore no
    /// error occurred in the send process.
    NoController,
    /// The component stopped within the timeout.
    Stopped,
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
    ///
    /// TODO(b/298106231): eventually this should become a sandbox and delivery map.
    pub namespace: NamespaceBuilder,

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

    /// An opaque token that represents the component instance.
    ///
    /// The `fuchsia.component/Introspector` protocol may be used to get the
    /// string moniker of the instance from this token.
    ///
    /// Runners may publish this token as part of diagnostics information, to
    /// identify the running component without knowing its moniker.
    ///
    /// The token is invalidated when the component instance is destroyed.
    pub component_instance: InstanceToken,
}

impl StartInfo {
    pub fn into_fidl(
        self,
        outgoing_server_end: ServerEnd<fio::DirectoryMarker>,
        runtime_server_end: ServerEnd<fio::DirectoryMarker>,
    ) -> Result<fcrunner::ComponentStartInfo, StartError> {
        let ns = self.namespace.serve().map_err(StartError::ServeNamespace)?;
        Ok(fcrunner::ComponentStartInfo {
            resolved_url: Some(self.resolved_url),
            program: Some(self.program),
            ns: Some(ns.into()),
            outgoing_dir: Some(outgoing_server_end),
            runtime_dir: Some(runtime_server_end),
            numbered_handles: Some(self.numbered_handles),
            encoded_config: self.encoded_config,
            break_on_start: self.break_on_start,
            component_instance: Some(self.component_instance.into()),
            ..Default::default()
        })
    }
}

/// Information and capabilities that the framework holds on behalf of a component,
/// to be delivered on the next execution.
#[derive(Debug, Default)]
pub struct EscrowRequest {
    // Escrow the outgoing directory server endpoint. Whenever the
    // component is started, the framework will return this channel via
    // `ComponentStartInfo.outgoing_dir`.
    pub outgoing_dir: Option<ServerEnd<fio::DirectoryMarker>>,

    /// Escrow some user defined state. Whenever the component is started,
    /// the framework will return these handles via
    /// `ComponentStartInfo.escrowed_dictionary`.
    pub escrowed_dictionary: Option<ClientEnd<fsandbox::DictionaryMarker>>,
}

impl From<fcrunner::ComponentControllerOnEscrowRequest> for EscrowRequest {
    fn from(value: fcrunner::ComponentControllerOnEscrowRequest) -> Self {
        Self { outgoing_dir: value.outgoing_dir, escrowed_dictionary: value.escrowed_dictionary }
    }
}

#[cfg(test)]
pub mod tests {
    use {
        super::*,
        crate::model::testing::mocks::{
            self, ControlMessage, ControllerActionResponse, MockController,
        },
        assert_matches::assert_matches,
        fidl::endpoints::ControlHandle,
        fuchsia_async as fasync,
        fuchsia_zircon::{self as zx, Koid},
        futures::lock::Mutex,
        std::{collections::HashMap, sync::Arc, task::Poll},
        zx::AsHandleRef,
    };

    #[fuchsia::test]
    /// Test scenario where we tell the controller to stop the component and
    /// the component stops immediately.
    async fn stop_component_well_behaved_component_stop() {
        // Create a mock program which simulates immediately shutting down
        // the component.
        let (program, server) = mocks::mock_program();
        let stop_timeout = zx::Duration::from_millis(50);
        let kill_timeout = zx::Duration::from_millis(10);

        // Create a request map which the MockController will fill with
        // requests it received related to mocked component.
        let requests: Arc<Mutex<HashMap<Koid, Vec<ControlMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let controller = MockController::new(server, requests.clone(), program.koid());
        controller.serve();

        let stop_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(stop_timeout));
            timer.await;
        });
        let kill_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(kill_timeout));
            timer.await;
        });
        match program.stop_or_kill_with_timeout(stop_timer, kill_timer).await {
            Ok(ComponentStopOutcome {
                request: StopRequestSuccess::Stopped,
                component_exit_status: zx::Status::OK,
            }) => {}
            Ok(result) => {
                panic!("unexpected successful stop result {:?}", result);
            }
            Err(e) => {
                panic!("unexpected error stopping component {:?}", e);
            }
        }

        let msg_map = requests.lock().await;
        let msg_list = msg_map.get(&program.koid()).expect("No messages received on the channel");

        // The controller should have only seen a STOP message since it stops
        // the component immediately.
        assert_eq!(msg_list, &vec![ControlMessage::Stop]);
    }

    #[fuchsia::test]
    /// Test where the control channel is already closed when we try to stop
    /// the component.
    async fn stop_component_successful_component_already_gone() {
        let (program, server) = mocks::mock_program();
        let stop_timeout = zx::Duration::from_millis(100);
        let kill_timeout = zx::Duration::from_millis(1);

        let stop_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(stop_timeout));
            timer.await;
        });
        let kill_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(kill_timeout));
            timer.await;
        });

        // Drop the server end so it closes
        drop(server);
        match program.stop_or_kill_with_timeout(stop_timer, kill_timer).await {
            Ok(ComponentStopOutcome {
                request: StopRequestSuccess::Stopped,
                component_exit_status: zx::Status::PEER_CLOSED,
            }) => {}
            Ok(result) => {
                panic!("unexpected successful stop result {:?}", result);
            }
            Err(e) => {
                panic!("unexpected error stopping component {:?}", e);
            }
        }
    }

    #[fuchsia::test]
    /// The scenario where the controller stops the component after a delay
    /// which is before the controller reaches its timeout.
    fn stop_component_successful_stop_with_delay() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        // Create a mock program which simulates shutting down the component
        // after a delay. The delay is much shorter than the period allotted
        // for the component to stop.
        let (program, server) = mocks::mock_program();
        let stop_timeout = zx::Duration::from_seconds(5);
        let kill_timeout = zx::Duration::from_millis(1);

        // Create a request map which the MockController will fill with
        // requests it received related to mocked component.
        let requests: Arc<Mutex<HashMap<Koid, Vec<ControlMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let component_stop_delay = zx::Duration::from_millis(stop_timeout.into_millis() / 1_000);
        let controller = MockController::new_with_responses(
            server,
            requests.clone(),
            program.koid(),
            // stop the component after 60ms
            ControllerActionResponse { close_channel: true, delay: Some(component_stop_delay) },
            ControllerActionResponse { close_channel: true, delay: Some(component_stop_delay) },
        );
        controller.serve();

        // Create the stop call that we expect to stop the component.
        let stop_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(stop_timeout));
            timer.await;
        });
        let kill_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(kill_timeout));
            timer.await;
        });
        let mut stop_future = Box::pin(program.stop_or_kill_with_timeout(stop_timer, kill_timer));

        // Poll the stop component future to where it has asked the controller
        // to stop the component. This should also cause the controller to
        // spawn the future with a delay to close the control channel.
        assert!(exec.run_until_stalled(&mut stop_future).is_pending());

        // Advance the clock beyond where the future to close the channel
        // should fire.
        let new_time =
            fasync::Time::from_nanos(exec.now().into_nanos() + component_stop_delay.into_nanos());
        exec.set_fake_time(new_time);
        exec.wake_expired_timers();

        // The controller channel should be closed so we can drive the stop
        // future to completion.
        match exec.run_until_stalled(&mut stop_future) {
            Poll::Ready(Ok(ComponentStopOutcome {
                request: StopRequestSuccess::Stopped,
                component_exit_status: zx::Status::OK,
            })) => {}
            Poll::Ready(Ok(result)) => {
                panic!("unexpected successful stop result {:?}", result);
            }
            Poll::Ready(Err(e)) => {
                panic!("unexpected error stopping component {:?}", e);
            }
            Poll::Pending => {
                panic!("future should have completed!");
            }
        }

        // Check that what we expect to be in the message map is there.
        let mut test_fut = Box::pin(async {
            let msg_map = requests.lock().await;
            let msg_list =
                msg_map.get(&program.koid()).expect("No messages received on the channel");

            // The controller should have only seen a STOP message since it stops
            // the component before the timeout is hit.
            assert_eq!(msg_list, &vec![ControlMessage::Stop]);
        });
        assert!(exec.run_until_stalled(&mut test_fut).is_ready());
    }

    #[fuchsia::test]
    /// Test scenario where the controller does not stop the component within
    /// the allowed period and the component stop state machine has to send
    /// the `kill` message to the controller. The runner then does not kill the
    /// component within the kill time out period.
    fn stop_component_successful_with_kill_timeout_result() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        // Create a controller which takes far longer than allowed to stop the
        // component.
        let (program, server) = mocks::mock_program();
        let stop_timeout = zx::Duration::from_seconds(5);
        let kill_timeout = zx::Duration::from_millis(200);

        // Create a request map which the MockController will fill with
        // requests it received related to mocked component.
        let requests: Arc<Mutex<HashMap<Koid, Vec<ControlMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stop_resp_delay = zx::Duration::from_millis(stop_timeout.into_millis() / 10);
        // since we want the mock controller to close the controller channel
        // before the kill timeout, set the response delay to less than the timeout
        let kill_resp_delay = zx::Duration::from_millis(kill_timeout.into_millis() * 2);
        let controller = MockController::new_with_responses(
            server,
            requests.clone(),
            program.koid(),
            // Process the stop message, but fail to close the channel. Channel
            // closure is the indication that a component stopped.
            ControllerActionResponse { close_channel: false, delay: Some(stop_resp_delay) },
            ControllerActionResponse { close_channel: true, delay: Some(kill_resp_delay) },
        );
        let mut mock_controller_future = Box::pin(controller.into_serve_future());

        let stop_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(stop_timeout));
            timer.await;
        });
        let kill_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(kill_timeout));
            timer.await;
        });
        let mut stop_fut = Box::pin(program.stop_or_kill_with_timeout(stop_timer, kill_timer));

        // The stop fn has sent the stop message and is now waiting for a response
        assert!(exec.run_until_stalled(&mut stop_fut).is_pending());
        assert!(exec.run_until_stalled(&mut mock_controller_future).is_pending());

        let mut check_msgs = Box::pin(async {
            // Check if the mock controller got all the messages we expected it to get.
            let msg_map = requests.lock().await;
            let msg_list =
                msg_map.get(&program.koid()).expect("No messages received on the channel");
            assert_eq!(msg_list, &vec![ControlMessage::Stop]);
        });
        assert!(exec.run_until_stalled(&mut check_msgs).is_ready());
        drop(check_msgs);

        // Roll time passed the stop timeout.
        let mut new_time =
            fasync::Time::from_nanos(exec.now().into_nanos() + stop_timeout.into_nanos());
        exec.set_fake_time(new_time);
        exec.wake_expired_timers();

        // Advance our futures, we expect the mock controller will do nothing
        // before the deadline
        assert!(exec.run_until_stalled(&mut mock_controller_future).is_pending());

        // The stop fn should now send a kill signal
        assert!(exec.run_until_stalled(&mut stop_fut).is_pending());

        // The kill signal should cause the mock controller to exit its loop
        // We still expect the mock controller to be holding responses
        assert!(exec.run_until_stalled(&mut mock_controller_future).is_ready());

        // The ComponentController should have sent the kill message by now
        let mut check_msgs = Box::pin(async {
            // Check if the mock controller got all the messages we expected it to get.
            let msg_map = requests.lock().await;
            let msg_list =
                msg_map.get(&program.koid()).expect("No messages received on the channel");
            assert_eq!(msg_list, &vec![ControlMessage::Stop, ControlMessage::Kill]);
        });
        assert!(exec.run_until_stalled(&mut check_msgs).is_ready());
        drop(check_msgs);

        // Roll time beyond the kill timeout period
        new_time = fasync::Time::from_nanos(exec.now().into_nanos() + kill_timeout.into_nanos());
        exec.set_fake_time(new_time);
        exec.wake_expired_timers();

        // The stop fn should have given up and returned a result
        assert_matches!(
            exec.run_until_stalled(&mut stop_fut),
            Poll::Ready(Ok(ComponentStopOutcome {
                request: StopRequestSuccess::KilledAfterTimeout,
                component_exit_status: zx::Status::TIMED_OUT,
            }))
        );

        drop(stop_fut);
        assert!(program.finalize().is_none());
    }

    #[fuchsia::test]
    /// Test scenario where the controller does not stop the component within
    /// the allowed period and the component stop state machine has to send
    /// the `kill` message to the controller. The controller then kills the
    /// component before the kill timeout is reached.
    fn stop_component_successful_with_kill_result() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        // Create a controller which takes far longer than allowed to stop the
        // component.
        let (program, server) = mocks::mock_program();
        let stop_timeout = zx::Duration::from_seconds(5);
        let kill_timeout = zx::Duration::from_millis(200);

        // Create a request map which the MockController will fill with
        // requests it received related to mocked component.
        let requests: Arc<Mutex<HashMap<Koid, Vec<ControlMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let kill_resp_delay = zx::Duration::from_millis(kill_timeout.into_millis() / 2);
        let controller = MockController::new_with_responses(
            server,
            requests.clone(),
            program.koid(),
            // Process the stop message, but fail to close the channel. Channel
            // closure is the indication that a component stopped.
            ControllerActionResponse { close_channel: false, delay: None },
            ControllerActionResponse { close_channel: true, delay: Some(kill_resp_delay) },
        );
        controller.serve();

        let stop_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(stop_timeout));
            timer.await;
        });
        let kill_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(kill_timeout));
            timer.await;
        });
        let mut stop_fut = Box::pin(program.stop_or_kill_with_timeout(stop_timer, kill_timer));

        // it should be the case we stall waiting for a response from the
        // controller
        assert!(exec.run_until_stalled(&mut stop_fut).is_pending());

        // Roll time passed the stop timeout.
        let mut new_time =
            fasync::Time::from_nanos(exec.now().into_nanos() + stop_timeout.into_nanos());
        exec.set_fake_time(new_time);
        exec.wake_expired_timers();
        assert!(exec.run_until_stalled(&mut stop_fut).is_pending());

        // Roll forward to where the mock controller should have closed the
        // controller channel.
        new_time = fasync::Time::from_nanos(exec.now().into_nanos() + kill_resp_delay.into_nanos());
        exec.set_fake_time(new_time);
        exec.wake_expired_timers();

        // At this point stop_component() will have completed, but the
        // controller's future was not polled to completion.
        assert_matches!(
            exec.run_until_stalled(&mut stop_fut),
            Poll::Ready(Ok(ComponentStopOutcome {
                request: StopRequestSuccess::Killed,
                component_exit_status: zx::Status::OK
            }))
        );

        drop(stop_fut);
        assert!(program.finalize().is_none());
    }

    #[fuchsia::test]
    /// In this case we expect success, but that the stop state machine races
    /// with the controller. The state machine's timer expires, but when it
    /// goes to send the kill message, it finds the control channel is closed,
    /// indicating the component stopped.
    fn stop_component_successful_race_with_controller() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        // Create a program which takes far longer than allowed to stop the
        // component.
        let (program, server) = mocks::mock_program();
        let stop_timeout = zx::Duration::from_seconds(5);
        let kill_timeout = zx::Duration::from_millis(1);

        // Create a request map which the MockController will fill with
        // requests it received related to mocked component.
        let requests: Arc<Mutex<HashMap<Koid, Vec<ControlMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let close_delta = zx::Duration::from_millis(10);
        let resp_delay =
            zx::Duration::from_millis(stop_timeout.into_millis() + close_delta.into_millis());
        let controller = MockController::new_with_responses(
            server,
            requests.clone(),
            program.koid(),
            // Process the stop message, but fail to close the channel after
            // the timeout of stop_component()
            ControllerActionResponse { close_channel: true, delay: Some(resp_delay) },
            // This is irrelevant because the controller should never receive
            // the kill message
            ControllerActionResponse { close_channel: true, delay: Some(resp_delay) },
        );
        controller.serve();

        let stop_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(stop_timeout));
            timer.await;
        });
        let kill_timer = Box::pin(async move {
            let timer = fasync::Timer::new(fasync::Time::after(kill_timeout));
            timer.await;
        });
        let epitaph_fut = program.on_terminate();
        let mut stop_fut = Box::pin(program.stop_or_kill_with_timeout(stop_timer, kill_timer));

        // it should be the case we stall waiting for a response from the
        // controller
        assert!(exec.run_until_stalled(&mut stop_fut).is_pending());

        // Roll time passed the stop timeout and beyond when the controller
        // will close the channel
        let new_time = fasync::Time::from_nanos(exec.now().into_nanos() + resp_delay.into_nanos());
        exec.set_fake_time(new_time);
        exec.wake_expired_timers();

        // This future waits for the client channel to close. This creates a
        // rendezvous between the controller's execution context and the test.
        // Without this the message map state may be inconsistent.
        let mut check_msgs = Box::pin(async {
            epitaph_fut.await;

            let msg_map = requests.lock().await;
            let msg_list =
                msg_map.get(&program.koid()).expect("No messages received on the channel");

            assert_eq!(msg_list, &vec![ControlMessage::Stop]);
        });

        // Expect the message check future to complete because the controller
        // should close the channel.
        assert!(exec.run_until_stalled(&mut check_msgs).is_ready());
        drop(check_msgs);

        // At this point stop_component() should now poll to completion because
        // the control channel is closed, but stop_component will perceive this
        // happening after its timeout expired.
        assert_matches!(
            exec.run_until_stalled(&mut stop_fut),
            Poll::Ready(Ok(ComponentStopOutcome {
                request: StopRequestSuccess::Killed,
                component_exit_status: zx::Status::OK
            }))
        );

        drop(stop_fut);
        assert!(program.finalize().is_none());
    }

    /// Test that `Program::finalize` will reap the `OnEscrow` event received from the
    /// `ComponentController`.
    #[fuchsia::test]
    async fn finalize_program() {
        let (program, server) = mocks::mock_program();
        let (stream, control) = server.into_stream_and_control_handle().unwrap();
        let (outgoing_dir_client, outgoing_dir_server) = fidl::endpoints::create_endpoints();

        control
            .send_on_escrow(fcrunner::ComponentControllerOnEscrowRequest {
                outgoing_dir: Some(outgoing_dir_server),
                ..Default::default()
            })
            .unwrap();
        control.shutdown();
        drop(control);
        drop(stream);
        program.on_terminate().await;

        let escrow = program.finalize();
        let received_outgoing_dir_server = escrow.unwrap().outgoing_dir.unwrap();
        assert_eq!(
            outgoing_dir_client.basic_info().unwrap().koid,
            received_outgoing_dir_server.basic_info().unwrap().related_koid
        );
    }
}
