// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::startup,
    anyhow::{anyhow, Context as _, Error},
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio, fidl_fuchsia_session as fsession,
    fuchsia_component::server::{ServiceFs, ServiceObjLocal},
    fuchsia_inspect_contrib::nodes::BoundedListNode,
    fuchsia_zircon as zx,
    futures::{lock::Mutex, StreamExt, TryFutureExt, TryStreamExt},
    std::sync::Arc,
    tracing::{error, warn},
    vfs::{execution_scope::ExecutionScope, remote::remote_boxed_with_type},
};

/// Maximum number of concurrent connections to the protocols served by SessionManager.
const MAX_CONCURRENT_CONNECTIONS: usize = 10_000;

/// The name for the inspect node that tracks session restart timestamps.
const DIANGNOSTICS_SESSION_STARTED_AT_NAME: &str = "session_started_at";

/// The max size for the session restart timestamps list.
const DIANGNOSTICS_SESSION_STARTED_AT_SIZE: usize = 100;

/// The name of the property for each entry in the session_started_at list for
/// the start timestamp.
const DIAGNOSTICS_TIME_PROPERTY_NAME: &str = "@time";

/// A request to connect to a protocol exposed by SessionManager.
pub enum IncomingRequest {
    Launcher(fsession::LauncherRequestStream),
    Restarter(fsession::RestarterRequestStream),
    Lifecycle(fsession::LifecycleRequestStream),
}

struct Diagnostics {
    /// A list of session start/restart timestamps.
    session_started_at: BoundedListNode,
}

impl Diagnostics {
    pub fn record_session_start(&mut self) {
        self.session_started_at
            .create_entry()
            .record_int(DIAGNOSTICS_TIME_PROPERTY_NAME, zx::Time::get_monotonic().into_nanos());
    }
}

/// State of a launched session.
struct SessionState {
    /// The component URL of the session.
    url: String,

    /// A proxy to the session's exposed directory.
    exposed_dir: fio::DirectoryProxy,
}

struct SessionManagerState {
    /// The component URL for the default session.
    default_session_url: Option<String>,

    /// State of a launched session.
    ///
    /// If set, the component has been created and started, but is not guaranteed to be running
    /// since it may be stopped through external means.
    session: Option<SessionState>,

    /// The realm in which sessions will be launched.
    realm: fcomponent::RealmProxy,

    /// Collection of diagnostics nodes.
    diagnostics: Diagnostics,
}

/// Manages the session lifecycle and provides services to control the session.
#[derive(Clone)]
pub struct SessionManager {
    state: Arc<Mutex<SessionManagerState>>,
}

impl SessionManager {
    /// Constructs a new SessionManager.
    ///
    /// # Parameters
    /// - `realm`: The realm in which sessions will be launched.
    pub fn new(
        realm: fcomponent::RealmProxy,
        inspector: &fuchsia_inspect::Inspector,
        default_session_url: Option<String>,
    ) -> Self {
        let session_started_at = BoundedListNode::new(
            inspector.root().create_child(DIANGNOSTICS_SESSION_STARTED_AT_NAME),
            DIANGNOSTICS_SESSION_STARTED_AT_SIZE,
        );
        let diagnostics = Diagnostics { session_started_at };
        let state = SessionManagerState { default_session_url, session: None, realm, diagnostics };
        SessionManager { state: Arc::new(Mutex::new(state)) }
    }

    /// Launch the session with the default session component URL, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the is no default session URL or the session could not be launched.
    pub async fn launch_default_session(&mut self) -> Result<(), Error> {
        let mut state = self.state.lock().await;
        let session_url = state
            .default_session_url
            .as_ref()
            .ok_or_else(|| anyhow!("no default session URL configured"))?;
        let exposed_dir = startup::launch_session(&session_url, &state.realm).await?;
        state.session = Some(SessionState { url: session_url.clone(), exposed_dir });
        state.diagnostics.record_session_start();
        Ok(())
    }

    /// Starts serving [`IncomingRequest`] from `svc`.
    ///
    /// This will return once the [`ServiceFs`] stops serving requests.
    ///
    /// # Errors
    /// Returns an error if there is an issue serving the `svc` directory handle.
    pub async fn serve(
        &mut self,
        fs: &mut ServiceFs<ServiceObjLocal<'_, IncomingRequest>>,
    ) -> Result<(), Error> {
        fs.dir("svc")
            .add_fidl_service(IncomingRequest::Launcher)
            .add_fidl_service(IncomingRequest::Restarter)
            .add_fidl_service(IncomingRequest::Lifecycle);

        // Requests to /svc_from_session are forwarded to the session's exposed dir.
        let session_manager = self.clone();
        fs.add_entry_at(
            "svc_from_session",
            remote_boxed_with_type(
                Box::new(move |scope, flags, path, server_end| {
                    session_manager.open_svc_for_session(scope, flags, path, server_end);
                }),
                fio::DirentType::Directory,
            ),
        );

        fs.take_and_serve_directory_handle()?;

        fs.for_each_concurrent(MAX_CONCURRENT_CONNECTIONS, |request| {
            let mut session_manager = self.clone();
            async move {
                session_manager
                    .handle_incoming_request(request)
                    .unwrap_or_else(|err| error!(?err))
                    .await
            }
        })
        .await;

        Ok(())
    }

    /// Handles an [`IncomingRequest`].
    ///
    /// This will return once the protocol connection has been closed.
    ///
    /// # Errors
    /// Returns an error if there is an issue serving the request.
    async fn handle_incoming_request(&mut self, request: IncomingRequest) -> Result<(), Error> {
        match request {
            IncomingRequest::Launcher(request_stream) => {
                self.handle_launcher_request_stream(request_stream)
                    .await
                    .context("Session Launcher request stream got an error.")?;
            }
            IncomingRequest::Restarter(request_stream) => {
                self.handle_restarter_request_stream(request_stream)
                    .await
                    .context("Session Restarter request stream got an error.")?;
            }
            IncomingRequest::Lifecycle(request_stream) => {
                self.handle_lifecycle_request_stream(request_stream)
                    .await
                    .context("Session Lifecycle request stream got an error.")?;
            }
        }

        Ok(())
    }

    /// Handles a fuchsia.io.Directory/Open request for the /svc_from_session directory,
    /// forwarding the request to the session's exposed directory.
    ///
    /// If the session is not started, closes `server_end` with a PEER_CLOSED epitaph.
    fn open_svc_for_session(
        &self,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: vfs::path::Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let state = self.state.clone();
        scope.spawn(async move {
            let state = state.lock().await;
            match state.session.as_ref() {
                Some(session) => {
                    let _ = session.exposed_dir.open(
                        flags,
                        fio::ModeType::empty(),
                        path.as_ref(),
                        server_end,
                    );
                }
                None => {
                    warn!(
                        path = path.as_ref(),
                        "Failed to open protocol exposed by session: session has not been started."
                    );
                    server_end
                        .close_with_epitaph(zx::Status::PEER_CLOSED)
                        .unwrap_or_else(|err| error!(?err, "failed to send epitaph"));
                }
            }
        });
    }

    /// Serves a specified [`LauncherRequestStream`].
    ///
    /// # Parameters
    /// - `request_stream`: the LauncherRequestStream.
    ///
    /// # Errors
    /// When an error is encountered reading from the request stream.
    pub async fn handle_launcher_request_stream(
        &mut self,
        mut request_stream: fsession::LauncherRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) =
            request_stream.try_next().await.context("Error handling Launcher request stream")?
        {
            match request {
                fsession::LauncherRequest::Launch { configuration, responder } => {
                    let result = self.handle_launch_request(configuration).await;
                    let _ = responder.send(result);
                }
            };
        }
        Ok(())
    }

    /// Serves a specified [`RestarterRequestStream`].
    ///
    /// # Parameters
    /// - `request_stream`: the RestarterRequestStream.
    ///
    /// # Errors
    /// When an error is encountered reading from the request stream.
    pub async fn handle_restarter_request_stream(
        &mut self,
        mut request_stream: fsession::RestarterRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) =
            request_stream.try_next().await.context("Error handling Restarter request stream")?
        {
            match request {
                fsession::RestarterRequest::Restart { responder } => {
                    let result = self.handle_restart_request().await;
                    let _ = responder.send(result);
                }
            };
        }
        Ok(())
    }

    /// Serves a specified [`LifecycleRequestStream`].
    ///
    /// # Parameters
    /// - `request_stream`: the LifecycleRequestStream.
    ///
    /// # Errors
    /// When an error is encountered reading from the request stream.
    pub async fn handle_lifecycle_request_stream(
        &mut self,
        mut request_stream: fsession::LifecycleRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) =
            request_stream.try_next().await.context("Error handling Lifecycle request stream")?
        {
            match request {
                fsession::LifecycleRequest::Start { payload, responder } => {
                    let result = self.handle_lifecycle_start_request(payload.session_url).await;
                    let _ = responder.send(result);
                }
                fsession::LifecycleRequest::Stop { responder } => {
                    let result = self.handle_lifecycle_stop_request().await;
                    let _ = responder.send(result);
                }
                fsession::LifecycleRequest::Restart { responder } => {
                    let result = self.handle_lifecycle_restart_request().await;
                    let _ = responder.send(result);
                }
                fsession::LifecycleRequest::_UnknownMethod { ordinal, .. } => {
                    warn!(%ordinal, "Lifecycle received an unknown method");
                }
            };
        }
        Ok(())
    }

    /// Handles calls to Launcher.Launch().
    ///
    /// # Parameters
    /// - configuration: The launch configuration for the new session.
    async fn handle_launch_request(
        &mut self,
        configuration: fsession::LaunchConfiguration,
    ) -> Result<(), fsession::LaunchError> {
        let session_url = configuration.session_url.ok_or(fsession::LaunchError::InvalidArgs)?;
        let mut state = self.state.lock().await;
        startup::launch_session(&session_url, &state.realm).await.map_err(Into::into).map(
            |exposed_dir| {
                state.session = Some(SessionState { url: session_url, exposed_dir });
                state.diagnostics.record_session_start();
            },
        )
    }

    /// Handles a Restarter.Restart() request.
    async fn handle_restart_request(&mut self) -> Result<(), fsession::RestartError> {
        let mut state = self.state.lock().await;
        let session_url = &state.session.as_ref().ok_or(fsession::RestartError::NotRunning)?.url;
        startup::launch_session(&session_url, &state.realm).await.map_err(Into::into).map(
            |exposed_dir| {
                state.session.as_mut().unwrap().exposed_dir = exposed_dir;
                state.diagnostics.record_session_start();
            },
        )
    }

    /// Handles a `Lifecycle.Start()` request.
    ///
    /// # Parameters
    /// - session_url: The component URL for the session to start.
    async fn handle_lifecycle_start_request(
        &mut self,
        session_url: Option<String>,
    ) -> Result<(), fsession::LifecycleError> {
        let mut state = self.state.lock().await;
        if state.session.is_some() {
            return Err(fsession::LifecycleError::AlreadyStarted);
        }
        let session_url = session_url
            .as_ref()
            .or(state.default_session_url.as_ref())
            .ok_or(fsession::LifecycleError::NotFound)?
            .to_owned();
        startup::launch_session(&session_url, &state.realm).await.map_err(Into::into).map(
            |exposed_dir| {
                state.session = Some(SessionState { url: session_url, exposed_dir });
                state.diagnostics.record_session_start();
            },
        )
    }

    /// Handles a `Lifecycle.Stop()` request.
    async fn handle_lifecycle_stop_request(&mut self) -> Result<(), fsession::LifecycleError> {
        let mut state = self.state.lock().await;
        startup::stop_session(&state.realm).await.map_err(Into::into)?;
        state.session = None;
        Ok(())
    }

    /// Handles a `Lifecycle.Restart()` request.
    async fn handle_lifecycle_restart_request(&mut self) -> Result<(), fsession::LifecycleError> {
        let mut state = self.state.lock().await;
        let session_url = &state.session.as_ref().ok_or(fsession::LifecycleError::NotFound)?.url;
        startup::launch_session(&session_url, &state.realm).await.map_err(Into::into).map(
            |exposed_dir| {
                state.session.as_mut().unwrap().exposed_dir = exposed_dir;
                state.diagnostics.record_session_start();
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use {
        super::SessionManager,
        anyhow::{anyhow, Error},
        fidl::endpoints::{
            create_proxy_and_stream, spawn_stream_handler, DiscoverableProtocolMarker,
        },
        fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio,
        fidl_fuchsia_session as fsession,
        fuchsia_inspect::{self, assert_data_tree, testing::AnyProperty},
        futures::channel::mpsc,
        futures::prelude::*,
        lazy_static::lazy_static,
        session_testing::{spawn_directory_server, spawn_noop_directory_server},
        test_util::Counter,
        vfs::execution_scope::ExecutionScope,
    };

    fn serve_launcher(session_manager: SessionManager) -> fsession::LauncherProxy {
        let (launcher_proxy, launcher_stream) =
            create_proxy_and_stream::<fsession::LauncherMarker>().unwrap();
        {
            let mut session_manager_ = session_manager.clone();
            fuchsia_async::Task::spawn(async move {
                session_manager_
                    .handle_launcher_request_stream(launcher_stream)
                    .await
                    .expect("Session launcher request stream got an error.");
            })
            .detach();
        }
        launcher_proxy
    }

    fn serve_restarter(session_manager: SessionManager) -> fsession::RestarterProxy {
        let (restarter_proxy, restarter_stream) =
            create_proxy_and_stream::<fsession::RestarterMarker>().unwrap();
        {
            let mut session_manager_ = session_manager.clone();
            fuchsia_async::Task::spawn(async move {
                session_manager_
                    .handle_restarter_request_stream(restarter_stream)
                    .await
                    .expect("Session restarter request stream got an error.");
            })
            .detach();
        }
        restarter_proxy
    }

    fn serve_lifecycle(session_manager: SessionManager) -> fsession::LifecycleProxy {
        let (lifecycle_proxy, lifecycle_stream) =
            create_proxy_and_stream::<fsession::LifecycleMarker>().unwrap();
        {
            let mut session_manager_ = session_manager.clone();
            fuchsia_async::Task::spawn(async move {
                session_manager_
                    .handle_lifecycle_request_stream(lifecycle_stream)
                    .await
                    .expect("Session lifecycle request stream got an error.");
            })
            .detach();
        }
        lifecycle_proxy
    }

    /// Verifies that Launcher.Launch creates a new session.
    #[fuchsia::test]
    async fn test_launch() {
        let session_url = "session";

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::DestroyChild { child: _, responder } => {
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl,
                    args: _,
                    responder,
                } => {
                    assert_eq!(decl.url.unwrap(), session_url);
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir { child: _, exposed_dir, responder } => {
                    spawn_noop_directory_server(exposed_dir);
                    let _ = responder.send(Ok(()));
                }
                _ => panic!("Realm handler received an unexpected request"),
            };
        })
        .unwrap();

        let inspector = fuchsia_inspect::Inspector::default();
        let session_manager = SessionManager::new(realm, &inspector, None);
        let launcher = serve_launcher(session_manager);

        assert!(launcher
            .launch(&fsession::LaunchConfiguration {
                session_url: Some(session_url.to_string()),
                ..Default::default()
            })
            .await
            .is_ok());
        assert_data_tree!(inspector, root: {
            session_started_at: {
                "0": {
                    "@time": AnyProperty
                }
            }
        });
    }

    /// Verifies that Restarter.Restart restarts an existing session.
    #[fuchsia::test]
    async fn test_restarter_restart() {
        let session_url = "session";

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::DestroyChild { child: _, responder } => {
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl,
                    args: _,
                    responder,
                } => {
                    assert_eq!(decl.url.unwrap(), session_url);
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir { child: _, exposed_dir, responder } => {
                    spawn_noop_directory_server(exposed_dir);
                    let _ = responder.send(Ok(()));
                }
                _ => panic!("Realm handler received an unexpected request"),
            };
        })
        .unwrap();

        let inspector = fuchsia_inspect::Inspector::default();
        let session_manager = SessionManager::new(realm, &inspector, None);
        let launcher = serve_launcher(session_manager.clone());
        let restarter = serve_restarter(session_manager);

        assert!(launcher
            .launch(&fsession::LaunchConfiguration {
                session_url: Some(session_url.to_string()),
                ..Default::default()
            })
            .await
            .expect("could not call Launch")
            .is_ok());

        assert!(restarter.restart().await.expect("could not call Restart").is_ok());

        assert_data_tree!(inspector, root: {
            session_started_at: {
                "0": {
                    "@time": AnyProperty
                },
                "1": {
                    "@time": AnyProperty
                }
            }
        });
    }

    /// Verifies that Launcher.Restart return an error if there is no running existing session.
    #[fuchsia::test]
    async fn test_restarter_restart_error_not_running() {
        let realm = spawn_stream_handler(move |_realm_request| async move {
            panic!("Realm should not receive any requests as there is no session to launch")
        })
        .unwrap();

        let inspector = fuchsia_inspect::Inspector::default();
        let session_manager = SessionManager::new(realm, &inspector, None);
        let restarter = serve_restarter(session_manager);

        assert_eq!(
            Err(fsession::RestartError::NotRunning),
            restarter.restart().await.expect("could not call Restart")
        );

        assert_data_tree!(inspector, root: {
            session_started_at: {}
        });
    }

    /// Verifies that Lifecycle.Start creates a new session.
    #[fuchsia::test]
    async fn test_start() {
        let session_url = "session";

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::DestroyChild { child: _, responder } => {
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl,
                    args: _,
                    responder,
                } => {
                    assert_eq!(decl.url.unwrap(), session_url);
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir { child: _, exposed_dir, responder } => {
                    spawn_noop_directory_server(exposed_dir);
                    let _ = responder.send(Ok(()));
                }
                _ => panic!("Realm handler received an unexpected request"),
            };
        })
        .unwrap();

        let inspector = fuchsia_inspect::Inspector::default();
        let session_manager = SessionManager::new(realm, &inspector, None);
        let lifecycle = serve_lifecycle(session_manager);

        assert!(lifecycle
            .start(&fsession::LifecycleStartRequest {
                session_url: Some(session_url.to_string()),
                ..Default::default()
            })
            .await
            .is_ok());
        assert_data_tree!(inspector, root: {
            session_started_at: {
                "0": {
                    "@time": AnyProperty
                }
            }
        });
    }

    /// Verifies that Lifecycle.Start starts the default session if no URL is provided.
    #[fuchsia::test]
    async fn test_start_default() {
        let default_session_url = "session";

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::DestroyChild { child: _, responder } => {
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl,
                    args: _,
                    responder,
                } => {
                    assert_eq!(decl.url.unwrap(), default_session_url);
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir { child: _, exposed_dir, responder } => {
                    spawn_noop_directory_server(exposed_dir);
                    let _ = responder.send(Ok(()));
                }
                _ => panic!("Realm handler received an unexpected request"),
            };
        })
        .unwrap();

        let inspector = fuchsia_inspect::Inspector::default();
        let session_manager =
            SessionManager::new(realm, &inspector, Some(default_session_url.to_owned()));
        let lifecycle = serve_lifecycle(session_manager);

        assert!(lifecycle
            .start(&fsession::LifecycleStartRequest { session_url: None, ..Default::default() })
            .await
            .is_ok());
        assert_data_tree!(inspector, root: {
            session_started_at: {
                "0": {
                    "@time": AnyProperty
                }
            }
        });
    }

    /// Verifies that Lifecycle.Stop stops an existing session by destroying its component.
    #[fuchsia::test]
    async fn test_stop_destroys_component() {
        lazy_static! {
            static ref NUM_DESTROY_CHILD_CALLS: Counter = Counter::new(0);
        }

        let session_url = "session";

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::DestroyChild { child: _, responder } => {
                    NUM_DESTROY_CHILD_CALLS.inc();
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl,
                    args: _,
                    responder,
                } => {
                    assert_eq!(decl.url.unwrap(), session_url);
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir { child: _, exposed_dir, responder } => {
                    spawn_noop_directory_server(exposed_dir);
                    let _ = responder.send(Ok(()));
                }
                _ => panic!("Realm handler received an unexpected request"),
            };
        })
        .unwrap();

        let inspector = fuchsia_inspect::Inspector::default();
        let session_manager = SessionManager::new(realm, &inspector, None);
        let lifecycle = serve_lifecycle(session_manager);

        assert!(lifecycle
            .start(&fsession::LifecycleStartRequest {
                session_url: Some(session_url.to_string()),
                ..Default::default()
            })
            .await
            .is_ok());
        // Start attempts to destroy any existing session first.
        assert_eq!(NUM_DESTROY_CHILD_CALLS.get(), 1);
        assert_data_tree!(inspector, root: {
            session_started_at: {
                "0": {
                    "@time": AnyProperty
                }
            }
        });

        assert!(lifecycle.stop().await.is_ok());
        assert_eq!(NUM_DESTROY_CHILD_CALLS.get(), 2);
    }

    /// Verifies that Lifecycle.Restart restarts an existing session.
    #[fuchsia::test]
    async fn test_lifecycle_restart() {
        let session_url = "session";

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::DestroyChild { child: _, responder } => {
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl,
                    args: _,
                    responder,
                } => {
                    assert_eq!(decl.url.unwrap(), session_url);
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir { child: _, exposed_dir, responder } => {
                    spawn_noop_directory_server(exposed_dir);
                    let _ = responder.send(Ok(()));
                }
                _ => panic!("Realm handler received an unexpected request"),
            };
        })
        .unwrap();

        let inspector = fuchsia_inspect::Inspector::default();
        let session_manager = SessionManager::new(realm, &inspector, None);
        let lifecycle = serve_lifecycle(session_manager.clone());

        assert!(lifecycle
            .start(&fsession::LifecycleStartRequest {
                session_url: Some(session_url.to_string()),
                ..Default::default()
            })
            .await
            .expect("could not call Launch")
            .is_ok());

        assert!(lifecycle.restart().await.expect("could not call Restart").is_ok());

        assert_data_tree!(inspector, root: {
            session_started_at: {
                "0": {
                    "@time": AnyProperty
                },
                "1": {
                    "@time": AnyProperty
                }
            }
        });
    }

    /// Verifies that `open_svc_for_session` connects to the session's exposed dir.
    #[fuchsia::test]
    async fn test_svc_from_session() -> Result<(), Error> {
        let session_url = "session";
        let svc_path = "foo";

        let (path_sender, mut path_receiver) = mpsc::channel(1);

        let session_exposed_dir_handler = move |directory_request| match directory_request {
            fio::DirectoryRequest::Open { path, .. } => {
                let mut path_sender = path_sender.clone();
                path_sender.try_send(path).unwrap();
            }
            _ => panic!("Directory handler received an unexpected request"),
        };

        let realm = spawn_stream_handler(move |realm_request| {
            let session_exposed_dir_handler = session_exposed_dir_handler.clone();
            async move {
                match realm_request {
                    fcomponent::RealmRequest::DestroyChild { responder, .. } => {
                        let _ = responder.send(Ok(()));
                    }
                    fcomponent::RealmRequest::CreateChild { responder, .. } => {
                        let _ = responder.send(Ok(()));
                    }
                    fcomponent::RealmRequest::OpenExposedDir { exposed_dir, responder, .. } => {
                        spawn_directory_server(exposed_dir, session_exposed_dir_handler);
                        let _ = responder.send(Ok(()));
                    }
                    _ => panic!("Realm handler received an unexpected request"),
                };
            }
        })?;

        let inspector = fuchsia_inspect::Inspector::default();
        let session_manager = SessionManager::new(realm, &inspector, None);
        let lifecycle = serve_lifecycle(session_manager.clone());

        lifecycle
            .start(&fsession::LifecycleStartRequest {
                session_url: Some(session_url.to_string()),
                ..Default::default()
            })
            .await?
            .map_err(|err| anyhow!("failed to start: {:?}", err))?;

        // Start connects to fuchsia.component.Binder to start the component.
        assert_eq!(path_receiver.next().await.unwrap(), fcomponent::BinderMarker::PROTOCOL_NAME);

        // Open an arbitrary node in the session's exposed dir.
        // The actual protocol does not matter because it's not being served.
        let (_client_end, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();

        let scope = ExecutionScope::new();
        session_manager.open_svc_for_session(
            scope,
            fio::OpenFlags::empty(),
            vfs::path::Path::validate_and_split(svc_path)?,
            server_end,
        );

        assert_eq!(path_receiver.next().await.unwrap(), svc_path);

        Ok(())
    }
}
