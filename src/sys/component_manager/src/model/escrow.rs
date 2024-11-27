// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::Debug;
use std::sync::Arc;

use cm_util::TaskGroup;
use fasync::Task;
use fidl::endpoints::{create_proxy, ServerEnd};
use futures::channel::{mpsc, oneshot};
use futures::{select, FutureExt, StreamExt};
use std::sync::Mutex;
use vfs::directory::entry::OpenRequest;
use vfs::remote::remote_dir;
use zx::AsHandleRef;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fuchsia_async as fasync};

use super::start::Start;
use crate::bedrock::program::EscrowRequest;
use crate::model::component::StartReason;
use errors::ActionError;

pub struct EscrowedState {
    pub outgoing_dir: ServerEnd<fio::DirectoryMarker>,
    pub escrowed_dictionary: Option<fsandbox::DictionaryRef>,
}

impl EscrowedState {
    /// Wait until the escrow needs a component's attention, e.g. the outgoing directory
    /// server endpoint is readable.
    pub async fn needs_attention(&self) {
        _ = fasync::OnSignals::new(self.outgoing_dir.channel(), zx::Signals::CHANNEL_READABLE)
            .await;
    }

    #[cfg(test)]
    pub fn outgoing_dir_closed() -> Self {
        let (_, outgoing_dir) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        Self { outgoing_dir, escrowed_dictionary: None }
    }
}

impl Debug for EscrowedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EscrowedState")
            .field("outgoing", &self.outgoing_dir.basic_info().unwrap().koid)
            .field(
                "escrowed_dictionary",
                &self.escrowed_dictionary.as_ref().map(|v| v.token.basic_info().unwrap().koid),
            )
            .finish()
    }
}

/// An [`Actor`] synchronizes four events regarding a component:
///
/// - The component is stopped, possibly escrowing its outgoing directory server endpoint.
/// - The component will be started, thus requiring an outgoing directory server endpoint.
/// - Someone needs to open an object from the outgoing directory of the component.
/// - The escrowed outgoing directory server endpoint is readable, thus requiring us to
///   start the component to handle it.
///
/// Internally it uses the actor pattern to process commands in a queue, while allowing
/// commands and readable signal monitoring to interrupt each other.
///
/// All operations are non-blocking with the exception of extracting the escrowed outgoing
/// directory server endpoint, thus reducing the risks of deadlocks.
pub struct Actor {
    sender: mpsc::UnboundedSender<Command>,
    outgoing_dir: Arc<Mutex<fio::DirectoryProxy>>,
}

impl Actor {
    /// Creates a new actor and returns a reference that can be used to queue
    /// commands.
    ///
    /// Also returns a task owning and running the actor. The task should
    /// typically be run in a non-blocking task group of the component.
    pub fn new(scope: &TaskGroup, starter: impl Start + Send + Sync + 'static) -> Actor {
        let (sender, receiver) = mpsc::unbounded();
        let (client, server) = create_proxy::<fio::DirectoryMarker>().unwrap();
        let escrow = EscrowedState { outgoing_dir: server, escrowed_dictionary: None };
        let outgoing_dir = Arc::new(Mutex::new(client));
        let actor = ActorImpl {
            starter: Arc::new(starter),
            outgoing_dir: outgoing_dir.clone(),
            nonblocking_start_task: TaskGroup::new(),
        };
        scope.spawn(actor.run(escrow, receiver));
        let handle = Actor { sender, outgoing_dir };
        handle
    }

    /// Stores some state on behalf of the component and starts the component if
    /// the state is urgent. Call this after the program has stopped.
    ///
    /// It's an error to call this twice without a `will_start` in-between.
    pub fn did_stop(&self, escrow: Option<EscrowRequest>) {
        _ = self.sender.unbounded_send(Command::DidStop(escrow));
    }

    /// Extracts state held on behalf of the component. Call this when the program
    /// is starting and needs escrowed state.
    ///
    /// It's an error to call this twice without a `did_stop` in-between.
    /// Returns `None` if the actor task is destroyed.
    pub async fn will_start(&self) -> Option<EscrowedState> {
        let (sender, receiver) = oneshot::channel();
        _ = self.sender.unbounded_send(Command::WillStart(sender));
        receiver.await.ok()
    }

    /// Forwards `open_request` to the outgoing directory of the component.  If the component is not
    /// started, this will cause the escrowed state to become urgent and the component to be
    /// started.
    pub fn open_outgoing(&self, open_request: OpenRequest<'_>) -> Result<(), zx::Status> {
        open_request.open_remote(remote_dir(Clone::clone(&*self.outgoing_dir.lock().unwrap())))
    }
}

enum Command {
    DidStop(Option<EscrowRequest>),
    WillStart(oneshot::Sender<EscrowedState>),
}

struct ActorImpl {
    starter: Arc<dyn Start + Send + Sync + 'static>,
    outgoing_dir: Arc<Mutex<fio::DirectoryProxy>>,

    // The actor monitors a `start_task`, a task to start the component, until
    // the escrow state is reaped. But the rest of the component start process
    // still runs for a while. We should not drop the task lest it cancels the
    // start process in an inconsistent state, so the rest of the `start_task`
    // is tracked here.
    nonblocking_start_task: TaskGroup,
}

#[derive(Debug)]
enum State {
    /// The component has stopped.
    Stopped { escrow: EscrowedState },
    /// The component is being started.
    Starting { escrow: EscrowedState, start_task: Task<Result<(), ActionError>> },
    /// The component's program is running.
    Started,
    /// The actor should exit because there are no more commands.
    Quit,
}

impl ActorImpl {
    async fn run(mut self, escrow: EscrowedState, mut receiver: mpsc::UnboundedReceiver<Command>) {
        let mut state = State::Stopped { escrow };
        loop {
            state = match state {
                State::Stopped { escrow } => self.run_stopped(escrow, &mut receiver).await,
                State::Starting { escrow, start_task } => {
                    self.run_starting(escrow, start_task, &mut receiver).await
                }
                State::Started => self.run_started(&mut receiver).await,
                State::Quit => break,
            };
        }
    }

    async fn run_stopped(
        &mut self,
        escrow: EscrowedState,
        receiver: &mut mpsc::UnboundedReceiver<Command>,
    ) -> State {
        select! {
            command = receiver.next() => {
                let Some(command) = command else { return State::Quit };
                match command {
                    // TODO(https://fxbug.dev/319095979): These panics can be avoided by
                    // centralizing more state transitions in a coordinator.
                    //
                    // The current overall component state machine never double stops or
                    // double starts a component, but there's no good way to represent
                    // that in the type system, yet. The need for panic could go away if
                    // we had a larger state machine managing component starting and
                    // stopping, which can simply ignore the next stop request if the
                    // component is already stopped.
                    Command::DidStop(_) => panic!("double stop"),
                    Command::WillStart(sender) => {
                        _ = sender.send(escrow);
                        return State::Started;
                    }
                }
            },
            _ = escrow.needs_attention().fuse() => {
                // If the escrow needs attention, schedule a start action.
                let starter = self.starter.clone();
                let start_task = fasync::Task::spawn(async move {
                    starter.ensure_started(&StartReason::OutgoingDirectory).await
                });
                return State::Starting{escrow, start_task};
            },
        }
    }

    async fn run_starting(
        &mut self,
        escrow: EscrowedState,
        start_task: Task<Result<(), ActionError>>,
        receiver: &mut mpsc::UnboundedReceiver<Command>,
    ) -> State {
        let mut start_task = start_task.fuse();
        select! {
            command = receiver.next() => {
                let Some(command) = command else { return State::Quit };
                match command {
                    Command::DidStop(_) => panic!("double stop"),
                    Command::WillStart(sender) => {
                        _ = sender.send(escrow);
                        // When the program will imminently start, it reaps the escrow state. We
                        // can assume the open requests will be handled. But the rest of the
                        // component start process still runs for a while. We should not drop
                        // the task lest it cancels the start process in an inconsistent state.
                        self.nonblocking_start_task.spawn(async move {
                            match start_task.await {
                                Ok(()) => {}
                                Err(err) => {
                                    tracing::warn!(
                                        "the program of the component started, but the rest of the \
                                         start procedure (e.g. starting eager children) failed: \
                                         {err}"
                                    );
                                }
                            }
                        });
                        return State::Started;
                    }
                }
            },
            start_result = start_task => {
                match start_result {
                    Ok(()) => panic!("start task must call will_start before finishing"),
                    Err(ref err) => {
                        // If the start action completes with an error, clear the escrow
                        // that was used to trigger the start action. Otherwise, we'll
                        // be continuously starting the component in a loop.
                        tracing::warn!(
                            "the escrowed state of the component is readable but the component \
                             failed to start: {err}"
                        );
                        return self.update_escrow(Default::default());
                    }
                }
            },
        }
    }

    async fn run_started(&mut self, receiver: &mut mpsc::UnboundedReceiver<Command>) -> State {
        let command = receiver.next().await;
        let Some(command) = command else { return State::Quit };
        match command {
            Command::DidStop(request) => {
                return self.update_escrow(request.unwrap_or_default());
            }
            Command::WillStart(_) => panic!("double start"),
        }
    }

    fn update_escrow(&mut self, request: EscrowRequest) -> State {
        let outgoing_dir = if let Some(server) = request.outgoing_dir {
            server
        } else {
            // No outgoing directory server endpoint was escrowed. Mint a new pair and
            // update our client counterpart.
            let (client, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            *self.outgoing_dir.lock().unwrap() = client;
            server
        };
        let escrow =
            EscrowedState { outgoing_dir, escrowed_dictionary: request.escrowed_dictionary };
        State::Stopped { escrow }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Weak};
    use std::task::Poll;

    use assert_matches::assert_matches;
    use async_trait::async_trait;
    use cm_util::TaskGroup;
    use fidl_fuchsia_io as fio;
    use fuchsia_async::{self as fasync, TestExecutor};
    use futures::channel::mpsc;
    use futures::lock::Mutex;
    use futures::StreamExt;
    use moniker::Moniker;
    use vfs::directory::entry::OpenRequest;
    use vfs::execution_scope::ExecutionScope;
    use vfs::ToObjectRequest;

    use crate::bedrock::program::EscrowRequest;
    use crate::framework::controller;
    use crate::model::component::{IncomingCapabilities, StartReason};
    use crate::model::start::Start;
    use errors::{ActionError, StartActionError};

    use super::{Actor, EscrowedState};

    struct MustNotStart;

    #[async_trait]
    impl Start for MustNotStart {
        async fn ensure_started_etc<'a>(
            &'a self,
            _reason: &'a StartReason,
            _execution_controller_task: Option<controller::ExecutionControllerTask>,
            _incoming: IncomingCapabilities,
        ) -> Result<(), ActionError> {
            panic!("test expected not to start the component");
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    #[should_panic(expected = "double start")]
    async fn double_start() {
        let task_group = TaskGroup::new();
        let actor = Actor::new(&task_group, MustNotStart);

        _ = actor.will_start().await;
        _ = actor.will_start().await;
        task_group.join().await;
    }

    #[fuchsia::test(allow_stalls = false)]
    #[should_panic(expected = "double stop")]
    async fn double_stop() {
        let task_group = TaskGroup::new();
        let actor = Actor::new(&task_group, MustNotStart);

        _ = actor.will_start().await;
        _ = actor.did_stop(None);
        _ = actor.did_stop(None);
        task_group.join().await;
    }

    struct MockStart {
        start_tx: mpsc::UnboundedSender<(StartReason, EscrowedState)>,
        actor: Mutex<Option<Weak<Actor>>>,
    }

    /// Creates an `Actor` that owns a `MockStart` and uses it to start the component.
    fn new_mock_start_actor(
        task_group: &TaskGroup,
        start_tx: mpsc::UnboundedSender<(StartReason, EscrowedState)>,
    ) -> Arc<Actor> {
        let mock_start = MockStart { start_tx, actor: Mutex::new(None) };
        let mock_start = Arc::new(mock_start);
        let actor = Actor::new(task_group, mock_start.clone());
        let actor = Arc::new(actor);
        *mock_start.actor.try_lock().unwrap() = Some(Arc::downgrade(&actor));
        actor
    }

    #[async_trait]
    impl Start for Arc<MockStart> {
        async fn ensure_started_etc<'a>(
            &'a self,
            reason: &'a StartReason,
            _execution_controller_task: Option<controller::ExecutionControllerTask>,
            _incoming: IncomingCapabilities,
        ) -> Result<(), ActionError> {
            let actor = self.actor.lock().await.as_ref().unwrap().clone();
            let escrow = actor.upgrade().unwrap().will_start().await.unwrap();
            self.start_tx.unbounded_send((reason.clone(), escrow)).unwrap();
            Ok(())
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn open_outgoing_while_stopped() {
        let task_group = TaskGroup::new();
        let (start_tx, mut start_rx) = mpsc::unbounded();
        let actor = new_mock_start_actor(&task_group, start_tx);

        let (_, server_end) = zx::Channel::create();

        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_eq!(
            actor.open_outgoing(OpenRequest::new(
                execution_scope.clone(),
                fio::OpenFlags::empty(),
                "foo".try_into().unwrap(),
                &mut object_request
            )),
            Ok(())
        );
        let (reason, escrow) = start_rx.next().await.unwrap();
        assert_eq!(reason, StartReason::OutgoingDirectory);

        let mut outgoing = escrow.outgoing_dir.into_stream();
        let dir_entry = outgoing.next().await.unwrap().unwrap().into_open().unwrap();
        assert_eq!(dir_entry.2, "foo");

        drop(actor);
        task_group.join().await;
        assert_matches!(start_rx.next().await, None);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn open_outgoing_while_running() {
        let task_group = TaskGroup::new();
        let (start_tx, mut start_rx) = mpsc::unbounded();
        let actor = new_mock_start_actor(&task_group, start_tx);

        let escrow = actor.will_start().await;
        assert!(escrow.is_some());

        let (_, server_end) = zx::Channel::create();
        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_eq!(
            actor.open_outgoing(OpenRequest::new(
                execution_scope.clone(),
                fio::OpenFlags::empty(),
                "foo".try_into().unwrap(),
                &mut object_request
            )),
            Ok(())
        );

        let mut next_start = start_rx.next();
        assert_matches!(TestExecutor::poll_until_stalled(&mut next_start).await, Poll::Pending);

        let mut outgoing = escrow.unwrap().outgoing_dir.into_stream();
        let open = outgoing.next().await.unwrap().unwrap().into_open().unwrap();
        assert_eq!(open.2, "foo");

        drop(actor);
        task_group.join().await;
        assert_matches!(start_rx.next().await, None);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn open_outgoing_before_stopped() {
        let task_group = TaskGroup::new();
        let (start_tx, mut start_rx) = mpsc::unbounded();
        let actor = new_mock_start_actor(&task_group, start_tx);

        let escrow = actor.will_start().await;
        assert!(escrow.is_some());
        assert_matches!(TestExecutor::poll_until_stalled(start_rx.next()).await, Poll::Pending);

        let (_, server_end) = zx::Channel::create();
        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_eq!(
            actor.open_outgoing(OpenRequest::new(
                execution_scope.clone(),
                fio::OpenFlags::empty(),
                "foo".try_into().unwrap(),
                &mut object_request
            )),
            Ok(())
        );
        assert_matches!(TestExecutor::poll_until_stalled(start_rx.next()).await, Poll::Pending);

        // Component stopped with an unread message. It should be started back up.
        actor.did_stop(Some(EscrowRequest {
            outgoing_dir: Some(escrow.unwrap().outgoing_dir),
            escrowed_dictionary: None,
        }));
        assert_matches!(TestExecutor::poll_until_stalled(start_rx.next()).await, Poll::Ready(_));

        drop(actor);
        task_group.join().await;
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn open_outgoing_after_stopped() {
        let task_group = TaskGroup::new();
        let (start_tx, mut start_rx) = mpsc::unbounded();
        let actor = new_mock_start_actor(&task_group, start_tx);

        let escrow = actor.will_start().await;
        assert!(escrow.is_some());
        assert_matches!(TestExecutor::poll_until_stalled(start_rx.next()).await, Poll::Pending);

        // Component stopped and then got an unread message. It should be started back up.
        actor.did_stop(Some(EscrowRequest {
            outgoing_dir: Some(escrow.unwrap().outgoing_dir),
            escrowed_dictionary: None,
        }));
        let (_, server_end) = zx::Channel::create();
        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_eq!(
            actor.open_outgoing(OpenRequest::new(
                execution_scope.clone(),
                fio::OpenFlags::empty(),
                "foo".try_into().unwrap(),
                &mut object_request
            )),
            Ok(())
        );
        assert_matches!(TestExecutor::poll_until_stalled(start_rx.next()).await, Poll::Ready(_));

        drop(actor);
        task_group.join().await;
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn stop_without_escrow() {
        let task_group = TaskGroup::new();
        let actor = Actor::new(&task_group, MustNotStart);

        let escrow = actor.will_start().await;
        let (client_end, server_end) = zx::Channel::create();
        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_eq!(
            actor.open_outgoing(OpenRequest::new(
                execution_scope.clone(),
                fio::OpenFlags::empty(),
                "foo".try_into().unwrap(),
                &mut object_request
            )),
            Ok(())
        );

        // Component stopped without escrowing anything. The open request will be lost.
        drop(escrow);
        actor.did_stop(None);
        fasync::OnSignals::new(&client_end, zx::Signals::CHANNEL_PEER_CLOSED).await.unwrap();

        // If the component is started again, it can receive requests again.
        let escrow = actor.will_start().await;
        let (_, server_end) = zx::Channel::create();
        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_eq!(
            actor.open_outgoing(OpenRequest::new(
                execution_scope.clone(),
                fio::OpenFlags::empty(),
                "bar".try_into().unwrap(),
                &mut object_request
            )),
            Ok(())
        );
        let mut outgoing = escrow.unwrap().outgoing_dir.into_stream();
        let open = outgoing.next().await.unwrap().unwrap().into_open().unwrap();
        assert_eq!(open.2, "bar");

        drop(actor);
        task_group.join().await;
    }

    struct BlockingStart {
        start_tx: mpsc::UnboundedSender<StartReason>,
        result_rx: Mutex<mpsc::UnboundedReceiver<Result<(), ActionError>>>,
    }

    #[async_trait]
    impl Start for BlockingStart {
        async fn ensure_started_etc<'a>(
            &'a self,
            reason: &'a StartReason,
            _execution_controller_task: Option<controller::ExecutionControllerTask>,
            _incoming: IncomingCapabilities,
        ) -> Result<(), ActionError> {
            self.start_tx.unbounded_send(reason.clone()).unwrap();
            let mut result_rx = self.result_rx.lock().await;
            result_rx.next().await.unwrap()
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_failed_before_reaping_escrow() {
        let task_group = TaskGroup::new();
        let (start_tx, mut start_rx) = mpsc::unbounded();
        let (result_tx, result_rx) = mpsc::unbounded();
        let actor =
            Actor::new(&task_group, BlockingStart { start_tx, result_rx: Mutex::new(result_rx) });

        let (client_end, server_end) = zx::Channel::create();
        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_eq!(
            actor.open_outgoing(OpenRequest::new(
                execution_scope.clone(),
                fio::OpenFlags::empty(),
                "foo".try_into().unwrap(),
                &mut object_request
            )),
            Ok(())
        );
        start_rx.next().await.unwrap();

        // Fail the start request.
        result_tx
            .unbounded_send(Err(ActionError::StartError {
                err: StartActionError::Aborted { moniker: Moniker::default() },
            }))
            .unwrap();

        // Connection got closed.
        fasync::OnSignals::new(&client_end, zx::Signals::CHANNEL_PEER_CLOSED).await.unwrap();

        drop(actor);
        task_group.join().await;
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_failed_after_reaping_escrow() {
        let task_group = TaskGroup::new();
        let (start_tx, mut start_rx) = mpsc::unbounded();
        let (result_tx, result_rx) = mpsc::unbounded();
        let actor =
            Actor::new(&task_group, BlockingStart { start_tx, result_rx: Mutex::new(result_rx) });

        let (_, server_end) = zx::Channel::create();
        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_eq!(
            actor.open_outgoing(OpenRequest::new(
                execution_scope.clone(),
                fio::OpenFlags::empty(),
                "foo".try_into().unwrap(),
                &mut object_request
            )),
            Ok(())
        );
        start_rx.next().await.unwrap();

        // Notify actor that the program is started. Open should already succeed.
        let escrow = actor.will_start().await;

        // Fail the rest of the start process. This doesn't matter to open.
        result_tx
            .unbounded_send(Err(ActionError::StartError {
                err: StartActionError::Aborted { moniker: Moniker::default() },
            }))
            .unwrap();

        let mut outgoing = escrow.unwrap().outgoing_dir.into_stream();
        let open = outgoing.next().await.unwrap().unwrap().into_open().unwrap();
        assert_eq!(open.2, "foo");

        drop(actor);
        task_group.join().await;
    }
}
