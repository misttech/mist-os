// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::{Context as _, Error},
    assert_matches::assert_matches,
    async_trait::async_trait,
    cap::Remote,
    exec::{elf, process, Lifecycle, Start, Stop},
    fidl::endpoints::{create_proxy, RequestStream},
    fidl_fuchsia_examples as fexamples, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_runtime::{HandleInfo, HandleType},
    fuchsia_zircon::{self as zx, AsHandleRef, HandleBased},
    futures::channel::mpsc,
    futures::lock::Mutex,
    futures::{StreamExt, TryStreamExt},
    lazy_static::lazy_static,
    process_builder::StartupHandle,
    processargs::ProcessArgs,
    std::ffi::CString,
    std::iter::once,
    std::mem,
    std::path::PathBuf,
    std::sync::Arc,
    test_util::Counter,
};

lazy_static! {
    /// Path to the `bedrock_testbed_hello` executable, relative to the test's namespace.
    static ref HELLO_BIN_PATH: PathBuf = PathBuf::from("/pkg/bin/bedrock_testbed_hello");

    /// Path to the `bedrock_testbed_echo_server_cap_multishot` executable, relative to the test's namespace.
    static ref ECHO_SERVER_CAP_MULTISHOT_BIN_PATH: PathBuf = PathBuf::from("/pkg/bin/bedrock_testbed_echo_server_cap_multishot");

    /// Path to the `bedrock_testbed_echo_client_cap` executable, relative to the test's namespace.
    static ref ECHO_CLIENT_CAP_BIN_PATH: PathBuf = PathBuf::from("/pkg/bin/bedrock_testbed_echo_client_cap");
}

/// The Dict key for the Sender capability that transmits Echo protocol server ends.
const ECHO_SENDER_CAP_NAME: &str = "echo_server_end_sender";

/// The Dict key for the Receiver capability that transmits Echo protocol server ends.
const ECHO_RECEIVER_CAP_NAME: &str = "echo_server_end_receiver";

/// A declaration of the program that `RestartableElf` creates when started.
///
/// Think of it as the `program` section of a component manifest.
struct Decl {
    executable: PathBuf,
}

enum State {
    // Not a real state. This is used as an intermediate value when replacing a state inside
    // of a Mutex, as a workaround for not being able to move out of &mut borrowed values.
    Unknown,

    Created(Created),
    Started(Started),
    Stopped,
}

/// State of an `RestartableElf` that has been created.
///
/// In this state, the process has been created, but not started.
struct Created {
    elf: elf::Elf,

    /// Task that serves the program's incoming Dict.
    serve_incoming: fasync::Task<()>,
}

impl Created {
    async fn create(decl: &Decl, incoming: Box<cap::CloneDict>) -> Result<Self, Error> {
        // Open the package directory.
        let pkg_dir = fuchsia_fs::directory::open_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .context("failed to open /pkg directory")?;

        let process_name = CString::new(
            decl.executable.file_name().unwrap().to_str().context("filename is not unicode")?,
        )
        .context("filename contains nul bytes")?;
        let executable = decl.executable.strip_prefix("/pkg")?.to_str().unwrap();

        // Convert the incoming `Dict` to a Zircon handle that we can pass to the elf.
        let (incoming_handle, incoming_fut) = incoming.to_zx_handle();

        // Serve the `Dict` protocol.
        let serve_incoming = fasync::Task::spawn(incoming_fut.unwrap());

        // Pass the incoming `Dict` to the process through processargs.
        let mut processargs = ProcessArgs::new();
        processargs.add_default_loader()?.add_handles(once(StartupHandle {
            handle: incoming_handle,
            info: HandleInfo::new(HandleType::User0, 0),
        }));

        let elf = elf::Elf::create_from_package(
            executable,
            pkg_dir,
            exec::process::CreateParams {
                name: process_name,
                job: fuchsia_runtime::job_default().duplicate(zx::Rights::SAME_RIGHTS)?,
                options: zx::ProcessOptions::empty(),
            },
            processargs,
        )
        .await?;

        Ok(Self { elf, serve_incoming })
    }
}

/// State of an `RestartableElf` that has been started.
///
/// In this state, the process has been started, but may not be running if it exited
/// or was killed by some external means.
struct Started {
    /// The Zircon process.
    process: process::Process,

    /// Task that serves the program's incoming Dict.
    serve_incoming: fasync::Task<()>,

    /// Task that sends the process' return code back to the `on_exit` caller.
    send_on_exit: fasync::Task<Result<(), Error>>,
}

impl Started {
    async fn create(created: Created, on_exit_sender: OnExitSender) -> Result<Self, Error> {
        let Created { elf, serve_incoming } = created;

        let process = elf.start().await.context("failed to start")?;

        let process_dup = process::Process(
            process
                .0
                .as_handle_ref()
                .duplicate(zx::Rights::SAME_RIGHTS)
                .context("failed to duplicate process handle")?
                .into(),
        );
        let send_on_exit = fasync::Task::spawn(async move {
            let return_code = process_dup.on_exit().await.context("failed to get return code")?;
            on_exit_sender.unbounded_send(return_code).context("failed to send return code")?;
            Ok(())
        });

        Ok(Self { process, serve_incoming, send_on_exit })
    }
}

/// The process return code.
type ReturnCode = <process::Process as exec::Lifecycle>::Exit;
/// The sender end of a channel that contains process return codes for Elf instances.
type OnExitSender = mpsc::UnboundedSender<ReturnCode>;
/// The receiver end of a channel that contains process return codes for Elf instances.
type OnExitReceiver = mpsc::UnboundedReceiver<ReturnCode>;

/// A stateful variant of `Elf` that can be restarted.
///
/// # States
///
/// `RestartableElf` is a state machine that begins in the `Created` state:
///
/// ```
///          start()          stop()
/// Created  ------>  Started ----->  Stopped
///                      ^--------------/
///                           start()
/// ```
///
/// ## `Created`
///
/// This has loaded the executable and holds the `Elf` instance that manages the process.
/// The process has not been started yet.
///
/// A `Created` state can move to the `Started` state with `start()`.
///
/// ## `Started`
///
/// The process is running.
///
/// A `Started` state can move to the `Stopped` state with `stop()`.
///
/// ## `Stopped`
///
/// The process is not running.
///
/// A `Stopped` state can move to the `Started` state with `start()`.
struct RestartableElf {
    decl: Decl,
    incoming: Box<cap::CloneDict>,
    on_exit_sender: OnExitSender,
    on_exit_receiver: OnExitReceiver,
    state: Arc<Mutex<State>>,
}

impl RestartableElf {
    pub async fn create(decl: Decl, incoming: Box<cap::CloneDict>) -> Result<Self, Error> {
        let (on_exit_sender, on_exit_receiver) = mpsc::unbounded();
        let created = Created::create(&decl, incoming.clone()).await?;

        Ok(RestartableElf {
            decl,
            incoming,
            on_exit_sender,
            on_exit_receiver,
            state: Arc::new(Mutex::new(State::Created(created))),
        })
    }

    /// Returns the return codes when the current or next Elf program launched by this
    /// RestartableElf exits.
    pub async fn on_exit(&mut self) -> Option<ReturnCode> {
        self.on_exit_receiver.next().await
    }
}

#[async_trait]
impl Start for RestartableElf {
    type Error = Error;
    type Stop = Self;

    async fn start(self) -> Result<Self::Stop, Self::Error> {
        {
            let mut state = self.state.lock().await;
            let previous_state = mem::replace(&mut *state, State::Unknown);
            *state = match previous_state {
                State::Created(created) => {
                    let started = Started::create(created, self.on_exit_sender.clone()).await?;
                    State::Started(started)
                }
                State::Started(started) => {
                    // No-op. Processes can't be restarted. To restart, the client should call
                    // `stop`, then `start`.
                    State::Started(started)
                }
                State::Stopped => {
                    let created = Created::create(&self.decl, self.incoming.clone()).await?;
                    let started = Started::create(created, self.on_exit_sender.clone()).await?;
                    State::Started(started)
                }
                _ => unimplemented!(),
            };
        }
        Ok(self)
    }
}

#[async_trait]
impl Stop for RestartableElf {
    type Error = Error;

    async fn stop(&mut self) -> Result<(), Self::Error> {
        let mut state = self.state.lock().await;
        let previous_state = mem::replace(&mut *state, State::Unknown);
        *state = match previous_state {
            State::Started(Started { mut process, serve_incoming, send_on_exit }) => {
                process.stop().await.context("failed to stop")?;
                serve_incoming.cancel().await;
                send_on_exit.await?;
                State::Stopped
            }
            _ => unimplemented!(),
        };
        Ok(())
    }
}

async fn handle_echo_requests(
    stream: fexamples::EchoRequestStream,
    echo_string_call_count: &'static Counter,
) -> Result<(), Error> {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                fexamples::EchoRequest::EchoString { value, responder } => {
                    echo_string_call_count.inc();
                    responder.send(&value).context("error sending EchoString response")?;
                }
                fexamples::EchoRequest::SendString { value, control_handle } => {
                    control_handle
                        .send_on_string(&value)
                        .context("error sending SendString event")?;
                }
            }
            Ok(())
        })
        .await
}

#[fuchsia::test]
async fn start_stop() -> Result<(), Error> {
    let incoming = Box::new(cap::CloneDict::new());
    let elf = RestartableElf::create(Decl { executable: HELLO_BIN_PATH.clone() }, incoming).await?;

    let mut started = elf.start().await?;

    started.stop().await?;

    Ok(())
}

#[fuchsia::test]
async fn restart_echo_client() -> Result<(), Error> {
    lazy_static! {
        static ref ECHO_STRING_CALL_COUNT: Counter = Counter::new(0);
    }

    // Create the "incoming" interface for the echo_client_cap program.
    // This is a Dict capability that contains a Sender that accepts Echo server ends.

    // TODO(fxbug.dev/122024): Send a more specific type like ServerEnd?
    let (echo_sender, mut echo_receiver) = cap::multishot::<cap::Handle>();

    let mut incoming = Box::new(cap::CloneDict::new());
    incoming.entries.insert(ECHO_SENDER_CAP_NAME.into(), Box::new(echo_sender));

    // Create and start the `RestartableElf`.
    let elf =
        RestartableElf::create(Decl { executable: ECHO_CLIENT_CAP_BIN_PATH.clone() }, incoming)
            .await?;
    let mut elf = elf.start().await?;

    // Receive Echo server ends and serve the Echo protocol on each one.
    let serve_echo = async move {
        while let Some(echo_server_end_handle) = echo_receiver.0.next().await {
            let echo_request_stream = fexamples::EchoRequestStream::from_channel(
                fasync::Channel::from_channel(
                    echo_server_end_handle.into_handle_based::<zx::Channel>(),
                )
                .expect("failed to create channel"),
            );

            fasync::Task::spawn(async move {
                handle_echo_requests(echo_request_stream, &ECHO_STRING_CALL_COUNT)
                    .await
                    .expect("failed to handle Echo requests");
            })
            .detach();
        }
    };
    fasync::Task::spawn(serve_echo).detach();

    // Ensure the client exits cleanly.
    let return_code = elf.on_exit().await;
    assert_matches!(return_code, Some(0));

    // The client should have called EchoString once.
    assert_eq!(ECHO_STRING_CALL_COUNT.get(), 1);

    // Stop the `RestartableElf`.
    elf.stop().await.context("failed to stop")?;

    // Start it again.
    let mut elf = elf.start().await?;

    // Ensure the client exits cleanly, again.
    let return_code = elf.on_exit().await;
    assert_matches!(return_code, Some(0));

    // The client should have also called EchoString once, so two in total.
    assert_eq!(ECHO_STRING_CALL_COUNT.get(), 2);

    Ok(())
}

#[fuchsia::test]
async fn restart_echo_server() -> Result<(), Error> {
    /// Connect to the Echo protocol by sending a server end over the Sender and call the
    /// EchoString method to verify it works.
    async fn connect_and_call_echo_string(
        echo_sender: &cap::multishot::Sender<cap::Handle>,
    ) -> Result<(), Error> {
        // Connect to the Echo protocol by sending the server end over the Sender.
        let (echo_proxy, echo_server_end) =
            create_proxy::<fexamples::EchoMarker>().context("failed to create Echo proxy")?;

        let echo_server_end_handle = cap::Handle::from(echo_server_end.into_handle());
        echo_sender
            .0
            .send(echo_server_end_handle)
            .await
            .context("failed to send Echo server end")?;

        // Call the EchoString method
        let message = "Hello, bedrock!";
        let response =
            echo_proxy.echo_string(message).await.context("failed to call EchoString")?;
        assert_eq!(response, message.to_string());

        Ok(())
    }

    // TODO(fxbug.dev/122024): Send a more specific type like ServerEnd?
    let (echo_sender, echo_receiver) = cap::multishot::<cap::Handle>();

    let mut incoming = Box::new(cap::CloneDict::new());
    incoming.entries.insert(ECHO_RECEIVER_CAP_NAME.into(), Box::new(echo_receiver));

    // Create and start the `RestartableElf`.
    let elf = RestartableElf::create(
        Decl { executable: ECHO_SERVER_CAP_MULTISHOT_BIN_PATH.clone() },
        incoming,
    )
    .await?;
    let mut elf = elf.start().await?;

    // Connect and call a method a few times.
    for _ in 0..3 {
        connect_and_call_echo_string(&echo_sender).await?;
    }

    // Stop the `RestartableElf`.
    elf.stop().await.context("failed to stop")?;

    // Start it again.
    let _elf = elf.start().await?;

    // Connect and call a method again.
    connect_and_call_echo_string(&echo_sender).await?;

    Ok(())
}
