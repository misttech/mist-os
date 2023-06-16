// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::startup,
    anyhow::{anyhow, Context as _, Error},
    fidl::endpoints::ProtocolMarker,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_element as felement,
    fidl_fuchsia_session as fsession, fidl_fuchsia_web as fweb,
    fuchsia_component::server::{ServiceFs, ServiceObjLocal},
    fuchsia_inspect_contrib::nodes::BoundedListNode,
    fuchsia_zircon as zx,
    futures::{lock::Mutex, StreamExt, TryFutureExt, TryStreamExt},
    std::{future::Future, sync::Arc},
    tracing::{error, warn},
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
    Manager(felement::ManagerRequestStream),
    GraphicalPresenter(felement::GraphicalPresenterRequestStream),
    Launcher(fsession::LauncherRequestStream),
    Restarter(fsession::RestarterRequestStream),
    Lifecycle(fsession::LifecycleRequestStream),
    WebDebug(fweb::DebugRequestStream),
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

    /// A client-end channel to the most session's `exposed_dir`.
    exposed_dir: zx::Channel,
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
            .add_fidl_service(IncomingRequest::Manager)
            .add_fidl_service(IncomingRequest::GraphicalPresenter)
            .add_fidl_service(IncomingRequest::Launcher)
            .add_fidl_service(IncomingRequest::Restarter)
            .add_fidl_service(IncomingRequest::WebDebug)
            .add_fidl_service(IncomingRequest::Lifecycle);
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

    /// Forwards the given request stream to a service inside the `session`.
    ///
    /// # Parameters
    /// - `request_stream`: The request stream being forwarded
    /// - `handler`: The function that actually matches on each request type and calls the
    ///    corresponding method on the given `Proxy`.
    async fn forward_request_to_session<RS, F, Fut>(
        &mut self,
        request_stream: RS,
        handler: F,
    ) -> Result<(), Error>
    where
        RS: fidl::endpoints::RequestStream,
        RS::Protocol: ProtocolMarker,
        F: Fn(RS, <RS::Protocol as ProtocolMarker>::Proxy) -> Fut,
        Fut: Future<Output = Result<(), Error>>,
    {
        let protocol_name = <RS::Protocol as ProtocolMarker>::DEBUG_NAME;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<RS::Protocol>()
            .with_context(|| format!("Failed to create_proxy for {}", protocol_name))?;
        {
            let state = self.state.lock().await;
            let session =
                state.session.as_ref().ok_or_else(|| anyhow!("Session is not started"))?;
            fdio::service_connect_at(
                &session.exposed_dir,
                protocol_name,
                server_end.into_channel(),
            )
            .with_context(|| format!("Failed to connect to {}", protocol_name))?;
        }
        handler(request_stream, proxy)
            .await
            .with_context(|| format!("{} request stream got an error", protocol_name))
    }

    /// Handles an [`IncomingRequest`].
    ///
    /// This will return once the protocol connection has been closed.
    ///
    /// # Errors
    /// Returns an error if there is an issue serving the request.
    async fn handle_incoming_request(&mut self, request: IncomingRequest) -> Result<(), Error> {
        match request {
            IncomingRequest::Manager(request_stream) => {
                // Connect to element.Manager served by the session.
                self.forward_request_to_session(
                    request_stream,
                    SessionManager::handle_manager_request_stream,
                )
                .await?;
            }
            IncomingRequest::GraphicalPresenter(request_stream) => {
                // Connect to GraphicalPresenter served by the session.
                self.forward_request_to_session(
                    request_stream,
                    SessionManager::handle_graphical_presenter_request_stream,
                )
                .await?;
            }
            IncomingRequest::WebDebug(request_stream) => {
                // Connect to `fuchsia.web.Debug` served by the session.
                self.forward_request_to_session(
                    request_stream,
                    SessionManager::handle_web_debug_request_stream,
                )
                .await?;
            }
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

    /// Serves a specified [`ManagerRequestStream`].
    ///
    /// # Parameters
    /// - `request_stream`: the ManagerRequestStream.
    /// - `manager_proxy`: the ManagerProxy that will handle the relayed commands.
    ///
    /// # Errors
    /// When an error is encountered reading from the request stream.
    pub async fn handle_manager_request_stream(
        mut request_stream: felement::ManagerRequestStream,
        manager_proxy: felement::ManagerProxy,
    ) -> Result<(), Error> {
        while let Some(request) =
            request_stream.try_next().await.context("Error handling Manager request stream")?
        {
            match request {
                felement::ManagerRequest::ProposeElement { spec, controller, responder } => {
                    let result = manager_proxy.propose_element(spec, controller).await?;
                    responder.send(result)?;
                }
            };
        }
        Ok(())
    }

    /// Serves a specified [`GraphicalPresenterRequestStream`].
    ///
    /// # Parameters
    /// - `request_stream`: the GraphicalPresenterRequestStream.
    /// - `graphical_presenter_proxy`: the GraphicalPresenterProxy that will handle the relayed commands.
    ///
    /// # Errors
    /// When an error is encountered reading from the request stream.
    pub async fn handle_graphical_presenter_request_stream(
        mut request_stream: felement::GraphicalPresenterRequestStream,
        graphical_presenter_proxy: felement::GraphicalPresenterProxy,
    ) -> Result<(), Error> {
        while let Some(request) = request_stream
            .try_next()
            .await
            .context("Error handling Graphical Presenter request stream")?
        {
            match request {
                felement::GraphicalPresenterRequest::PresentView {
                    view_spec,
                    annotation_controller,
                    view_controller_request,
                    responder,
                } => {
                    let result = graphical_presenter_proxy
                        .present_view(view_spec, annotation_controller, view_controller_request)
                        .await?;
                    responder.send(result)?;
                }
            };
        }
        Ok(())
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

    /// Serves a specified [`DebugRequestStream`].
    ///
    /// # Parameters
    /// - `request_stream`: the DebugRequestStream.
    /// - `debug_proxy`: the DebugProxy that will handle the relayed commands.
    ///
    /// # Errors
    /// When an error is encountered reading from the request stream.
    pub async fn handle_web_debug_request_stream(
        mut request_stream: fweb::DebugRequestStream,
        debug_proxy: fweb::DebugProxy,
    ) -> Result<(), Error> {
        while let Some(request) =
            request_stream.try_next().await.context("Error handling web.Debug request stream")?
        {
            match request {
                fweb::DebugRequest::EnableDevTools { listener, responder } => {
                    debug_proxy.enable_dev_tools(listener).await?;
                    let _ = responder.send();
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
        fidl::endpoints::{create_proxy_and_stream, create_request_stream, spawn_stream_handler},
        fidl_fuchsia_component as fcomponent, fidl_fuchsia_element as felement,
        fidl_fuchsia_session as fsession, fidl_fuchsia_web as fweb,
        fuchsia_inspect::{self, assert_data_tree, testing::AnyProperty},
        futures::prelude::*,
        lazy_static::lazy_static,
        session_testing::spawn_noop_directory_server,
        test_util::Counter,
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

    #[fuchsia::test]
    async fn handle_element_manager_request_stream_propagates_request_to_downstream_service() {
        let (local_proxy, local_request_stream) =
            create_proxy_and_stream::<felement::ManagerMarker>()
                .expect("Failed to create local Manager proxy and stream");

        let (downstream_proxy, mut downstream_request_stream) =
            create_proxy_and_stream::<felement::ManagerMarker>()
                .expect("Failed to create downstream Manager proxy and stream");

        let element_url = "element_url";
        let mut num_elements_proposed = 0;

        let local_server_fut =
            SessionManager::handle_manager_request_stream(local_request_stream, downstream_proxy);

        let downstream_server_fut = async {
            while let Some(request) = downstream_request_stream.try_next().await.unwrap() {
                match request {
                    felement::ManagerRequest::ProposeElement { spec, responder, .. } => {
                        num_elements_proposed += 1;
                        assert_eq!(Some(element_url.to_string()), spec.component_url);
                        let _ = responder.send(Ok(()));
                    }
                }
            }
        };

        let propose_and_drop_fut = async {
            local_proxy
                .propose_element(
                    felement::Spec {
                        component_url: Some(element_url.to_string()),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .expect("Failed to call ProposeElement")
                .expect("Failed to propose element");

            std::mem::drop(local_proxy); // Drop proxy to terminate `server_fut`.
        };

        let _ = future::join3(propose_and_drop_fut, local_server_fut, downstream_server_fut).await;

        assert_eq!(num_elements_proposed, 1);
    }

    #[fuchsia::test]
    async fn handle_web_debug_request_stream_propagates_request_to_downstream_service() {
        let (local_proxy, local_request_stream) = create_proxy_and_stream::<fweb::DebugMarker>()
            .expect("Failed to create local web.Debug proxy and stream");

        let (downstream_proxy, mut downstream_request_stream) =
            create_proxy_and_stream::<fweb::DebugMarker>()
                .expect("Failed to create downstream web.Debug proxy and stream");

        let mut listeners = Vec::new();

        let local_server_fut =
            SessionManager::handle_web_debug_request_stream(local_request_stream, downstream_proxy);

        let downstream_server_fut = async {
            while let Some(request) = downstream_request_stream.try_next().await.unwrap() {
                match request {
                    fweb::DebugRequest::EnableDevTools { listener, responder, .. } => {
                        listeners.push(listener);
                        let _ = responder.send();
                    }
                }
            }
        };

        let (listener, _listener_request_stream) =
            create_request_stream::<fweb::DevToolsListenerMarker>()
                .expect("Failed to create web.DevToolsListener proxy and stream");

        let enable_and_drop_fut = async {
            local_proxy.enable_dev_tools(listener).await.expect("Failed to call EnableDevTools");

            std::mem::drop(local_proxy); // Drop proxy to terminate `server_fut`.
        };

        let _ = future::join3(enable_and_drop_fut, local_server_fut, downstream_server_fut).await;

        assert_eq!(listeners.len(), 1);
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
}
