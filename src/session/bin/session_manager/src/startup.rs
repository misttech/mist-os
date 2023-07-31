// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::cobalt,
    fidl::endpoints::{create_proxy, ServerEnd},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio, fidl_fuchsia_session as fsession,
    fuchsia_async as fasync, fuchsia_zircon as zx, realm_management,
    thiserror::{self, Error},
    tracing::info,
};

/// Errors returned by calls startup functions.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum StartupError {
    #[error("Existing session not destroyed at \"{}/{}\": {:?}", collection, name, err)]
    NotDestroyed { name: String, collection: String, err: fcomponent::Error },

    #[error("Session {} not created at \"{}/{}\": {:?}", url, collection, name, err)]
    NotCreated { name: String, collection: String, url: String, err: fcomponent::Error },

    #[error(
        "Exposed directory of session {} at \"{}/{}\" not opened: {:?}",
        url,
        collection,
        name,
        err
    )]
    ExposedDirNotOpened { name: String, collection: String, url: String, err: fcomponent::Error },

    #[error("Session {} not launched at \"{}/{}\": {:?}", url, collection, name, err)]
    NotLaunched { name: String, collection: String, url: String, err: fcomponent::Error },
}

impl Into<fsession::LaunchError> for StartupError {
    fn into(self) -> fsession::LaunchError {
        match self {
            StartupError::NotDestroyed { .. } => fsession::LaunchError::DestroyComponentFailed,
            StartupError::NotCreated { err, .. } => match err {
                fcomponent::Error::InstanceCannotResolve => fsession::LaunchError::NotFound,
                _ => fsession::LaunchError::CreateComponentFailed,
            },
            StartupError::ExposedDirNotOpened { .. } => {
                fsession::LaunchError::CreateComponentFailed
            }
            StartupError::NotLaunched { .. } => fsession::LaunchError::CreateComponentFailed,
        }
    }
}

impl Into<fsession::RestartError> for StartupError {
    fn into(self) -> fsession::RestartError {
        match self {
            StartupError::NotDestroyed { .. } => fsession::RestartError::DestroyComponentFailed,
            StartupError::NotCreated { err, .. } => match err {
                fcomponent::Error::InstanceCannotResolve => fsession::RestartError::NotFound,
                _ => fsession::RestartError::CreateComponentFailed,
            },
            StartupError::ExposedDirNotOpened { .. } => {
                fsession::RestartError::CreateComponentFailed
            }
            StartupError::NotLaunched { .. } => fsession::RestartError::CreateComponentFailed,
        }
    }
}

impl Into<fsession::LifecycleError> for StartupError {
    fn into(self) -> fsession::LifecycleError {
        match self {
            StartupError::NotDestroyed { .. } => fsession::LifecycleError::DestroyComponentFailed,
            StartupError::NotCreated { err, .. } => match err {
                fcomponent::Error::InstanceCannotResolve => {
                    fsession::LifecycleError::ResolveComponentFailed
                }
                _ => fsession::LifecycleError::CreateComponentFailed,
            },
            StartupError::ExposedDirNotOpened { .. } => {
                fsession::LifecycleError::CreateComponentFailed
            }
            StartupError::NotLaunched { .. } => fsession::LifecycleError::CreateComponentFailed,
        }
    }
}

/// The name of the session child component.
const SESSION_NAME: &str = "session";

/// The name of the child collection the session is added to, must match the declaration in
/// session_manager.cml.
const SESSION_CHILD_COLLECTION: &str = "session";

/// Launches the specified session.
///
/// Any existing session child will be destroyed prior to launching the new session.
///
/// Returns a controller for the session component, or an error.
///
/// # Parameters
/// - `session_url`: The URL of the session to launch.
/// - `exposed_dir`: The server end on which to serve the session's exposed directory.
/// - `realm`: The realm in which to launch the session.
///
/// # Errors
/// If there was a problem creating or binding to the session component instance.
pub async fn launch_session(
    session_url: &str,
    exposed_dir: ServerEnd<fio::DirectoryMarker>,
    realm: &fcomponent::RealmProxy,
) -> Result<fcomponent::ExecutionControllerProxy, StartupError> {
    info!(session_url, "Launching session");

    let start_time = zx::Time::get_monotonic();
    let controller = set_session(&session_url, realm, exposed_dir).await?;
    let end_time = zx::Time::get_monotonic();

    fasync::Task::local(async move {
        if let Ok(cobalt_logger) = cobalt::get_logger() {
            // The result is disregarded as there is not retry-logic if it fails, and the error is
            // not meant to be fatal.
            let _ = cobalt::log_session_launch_time(cobalt_logger, start_time, end_time).await;
        }
    })
    .detach();

    Ok(controller)
}

/// Stops the current session, if any.
///
/// # Parameters
/// - `realm`: The realm in which the session exists.
///
/// # Errors
/// `StartupError::NotDestroyed` if the session component could not be destroyed.
pub async fn stop_session(realm: &fcomponent::RealmProxy) -> Result<(), StartupError> {
    realm_management::destroy_child_component(SESSION_NAME, SESSION_CHILD_COLLECTION, realm)
        .await
        .map_err(|err| StartupError::NotDestroyed {
            name: SESSION_NAME.to_string(),
            collection: SESSION_CHILD_COLLECTION.to_string(),
            err,
        })
}

/// Sets the currently active session.
///
/// If an existing session is running, the session's component instance will be destroyed prior to
/// creating the new session, effectively replacing the session.
///
/// # Parameters
/// - `session_url`: The URL of the session to instantiate.
/// - `realm`: The realm in which to create the session.
/// - `exposed_dir`: The server end on which the session's exposed directory will be served.
///
/// # Errors
/// Returns an error if any of the realm operations fail, or the realm is unavailable.
async fn set_session(
    session_url: &str,
    realm: &fcomponent::RealmProxy,
    exposed_dir: ServerEnd<fio::DirectoryMarker>,
) -> Result<fcomponent::ExecutionControllerProxy, StartupError> {
    realm_management::destroy_child_component(SESSION_NAME, SESSION_CHILD_COLLECTION, realm)
        .await
        .or_else(|err: fcomponent::Error| match err {
            // Since the intent is simply to clear out the existing session child if it exists,
            // related errors are disregarded.
            fcomponent::Error::InvalidArguments
            | fcomponent::Error::InstanceNotFound
            | fcomponent::Error::CollectionNotFound => Ok(()),
            _ => Err(err),
        })
        .map_err(|err| StartupError::NotDestroyed {
            name: SESSION_NAME.to_string(),
            collection: SESSION_CHILD_COLLECTION.to_string(),
            err,
        })?;

    let (controller, controller_server_end) =
        create_proxy::<fcomponent::ControllerMarker>().unwrap();
    let create_child_args = fcomponent::CreateChildArgs {
        controller: Some(controller_server_end),
        ..Default::default()
    };
    realm_management::create_child_component(
        SESSION_NAME,
        &session_url,
        SESSION_CHILD_COLLECTION,
        create_child_args,
        realm,
    )
    .await
    .map_err(|err| StartupError::NotCreated {
        name: SESSION_NAME.to_string(),
        collection: SESSION_CHILD_COLLECTION.to_string(),
        url: session_url.to_string(),
        err,
    })?;

    realm_management::open_child_component_exposed_dir(
        SESSION_NAME,
        SESSION_CHILD_COLLECTION,
        realm,
        exposed_dir,
    )
    .await
    .map_err(|err| StartupError::ExposedDirNotOpened {
        name: SESSION_NAME.to_string(),
        collection: SESSION_CHILD_COLLECTION.to_string(),
        url: session_url.to_string(),
        err,
    })?;

    // Start the component.
    let (execution_controller, execution_controller_server_end) =
        create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    controller
        .start(fcomponent::StartChildArgs::default(), execution_controller_server_end)
        .await
        .map_err(|_| fcomponent::Error::Internal)
        .and_then(std::convert::identity)
        .map_err(|_err| StartupError::NotLaunched {
            name: SESSION_NAME.to_string(),
            collection: SESSION_CHILD_COLLECTION.to_string(),
            url: session_url.to_string(),
            err: fcomponent::Error::InstanceCannotStart,
        })?;

    Ok(execution_controller)
}

#[cfg(test)]
mod tests {
    use {
        super::{set_session, stop_session, SESSION_CHILD_COLLECTION, SESSION_NAME},
        anyhow::Error,
        fidl::endpoints::{create_endpoints, spawn_stream_handler},
        fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio,
        lazy_static::lazy_static,
        session_testing::{spawn_directory_server, spawn_server},
        test_util::Counter,
    };

    #[fuchsia::test]
    async fn set_session_calls_realm_methods_in_appropriate_order() -> Result<(), Error> {
        lazy_static! {
            // The number of realm calls which have been made so far.
            static ref NUM_REALM_REQUESTS: Counter = Counter::new(0);
        }

        let session_url = "session";

        let directory_request_handler = move |directory_request| match directory_request {
            fio::DirectoryRequest::Open { path: _, .. } => {
                assert_eq!(NUM_REALM_REQUESTS.get(), 4);
            }
            _ => panic!("Directory handler received an unexpected request"),
        };

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::DestroyChild { child, responder } => {
                    assert_eq!(NUM_REALM_REQUESTS.get(), 0);
                    assert_eq!(child.collection, Some(SESSION_CHILD_COLLECTION.to_string()));
                    assert_eq!(child.name, SESSION_NAME);

                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::CreateChild { collection, decl, args, responder } => {
                    assert_eq!(NUM_REALM_REQUESTS.get(), 1);
                    assert_eq!(decl.url.unwrap(), session_url);
                    assert_eq!(decl.name.unwrap(), SESSION_NAME);
                    assert_eq!(&collection.name, SESSION_CHILD_COLLECTION);

                    spawn_server(args.controller.unwrap(), move |controller_request| {
                        match controller_request {
                            fcomponent::ControllerRequest::Start { responder, .. } => {
                                let _ = responder.send(Ok(()));
                            }
                            fcomponent::ControllerRequest::IsStarted { .. } => unimplemented!(),
                        }
                    });

                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir { child, exposed_dir, responder } => {
                    assert_eq!(NUM_REALM_REQUESTS.get(), 2);
                    assert_eq!(child.collection, Some(SESSION_CHILD_COLLECTION.to_string()));
                    assert_eq!(child.name, SESSION_NAME);

                    spawn_directory_server(exposed_dir, directory_request_handler);
                    let _ = responder.send(Ok(()));
                }
                _ => panic!("Realm handler received an unexpected request"),
            };
            NUM_REALM_REQUESTS.inc();
        })?;

        let (_exposed_dir, exposed_dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        let _controller = set_session(session_url, &realm, exposed_dir_server_end).await?;

        Ok(())
    }

    #[fuchsia::test]
    async fn set_session_starts_component() -> Result<(), Error> {
        lazy_static! {
            static ref NUM_START_CALLS: Counter = Counter::new(0);
        }

        let session_url = "session";

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::DestroyChild { responder, .. } => {
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::CreateChild { args, responder, .. } => {
                    spawn_server(args.controller.unwrap(), move |controller_request| {
                        match controller_request {
                            fcomponent::ControllerRequest::Start { responder, .. } => {
                                NUM_START_CALLS.inc();
                                let _ = responder.send(Ok(()));
                            }
                            fcomponent::ControllerRequest::IsStarted { .. } => unimplemented!(),
                        }
                    });
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir { responder, .. } => {
                    let _ = responder.send(Ok(()));
                }
                _ => panic!("Realm handler received an unexpected request"),
            };
        })?;

        let (_exposed_dir, exposed_dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        let _controller = set_session(session_url, &realm, exposed_dir_server_end).await?;
        assert_eq!(NUM_START_CALLS.get(), 1);

        Ok(())
    }

    #[fuchsia::test]
    async fn stop_session_calls_destroy_child() -> Result<(), Error> {
        lazy_static! {
            static ref NUM_DESTROY_CHILD_CALLS: Counter = Counter::new(0);
        }

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::DestroyChild { child, responder } => {
                    assert_eq!(NUM_DESTROY_CHILD_CALLS.get(), 0);
                    assert_eq!(child.collection, Some(SESSION_CHILD_COLLECTION.to_string()));
                    assert_eq!(child.name, SESSION_NAME);

                    let _ = responder.send(Ok(()));
                }
                _ => panic!("Realm handler received an unexpected request"),
            };
            NUM_DESTROY_CHILD_CALLS.inc();
        })?;

        stop_session(&realm).await?;
        assert_eq!(NUM_DESTROY_CHILD_CALLS.get(), 1);

        Ok(())
    }
}
