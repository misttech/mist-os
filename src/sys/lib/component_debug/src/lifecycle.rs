// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_sys2 as fsys,
    fuchsia_url::AbsoluteComponentUrl,
    futures::{future::BoxFuture, FutureExt, StreamExt},
    moniker::RelativeMoniker,
    thiserror::Error,
};

/// Errors that apply to all lifecycle actions.
#[derive(Error, Debug)]
pub enum ActionError {
    #[error("the instance could not be found")]
    InstanceNotFound,
    #[error("component manager could not parse the moniker")]
    BadMoniker,
    #[error("component manager encountered an internal error")]
    Internal,
    #[error("component manager responded with an unknown error code")]
    UnknownError,
    #[error("unexpected FIDL error with LifecycleController: {0}")]
    Fidl(#[from] fidl::Error),
}

#[derive(Error, Debug)]
pub enum CreateError {
    #[error("the instance already exists")]
    InstanceAlreadyExists,
    #[error("component manager could not parse the given child declaration")]
    BadChildDecl,
    #[error("the parent instance does not have a collection with the given name")]
    CollectionNotFound,
    #[error(transparent)]
    ActionError(#[from] ActionError),
}

#[derive(Error, Debug)]
pub enum DestroyError {
    #[error("component manager could not parse the given child reference")]
    BadChildRef,
    #[error(transparent)]
    ActionError(#[from] ActionError),
}

#[derive(Error, Debug)]
pub enum StartError {
    #[error("the package identified by the instance URL could not be found")]
    PackageNotFound,
    #[error("the manifest for the instance could not be found in its package")]
    ManifestNotFound,
    #[error(transparent)]
    ActionError(#[from] ActionError),
}

#[derive(Error, Debug)]
pub enum ResolveError {
    #[error("the package identified by the instance URL could not be found")]
    PackageNotFound,
    #[error("the manifest for the instance could not be found in its package")]
    ManifestNotFound,
    #[error(transparent)]
    ActionError(#[from] ActionError),
}

/// Uses the `fuchsia.sys2.LifecycleController` protocol to create a dynamic component instance
/// with the given `moniker` and `url`.
pub async fn create_instance_in_collection(
    lifecycle_controller: &fsys::LifecycleControllerProxy,
    parent: &RelativeMoniker,
    collection: &str,
    child_name: &str,
    url: &AbsoluteComponentUrl,
    config_overrides: Vec<fdecl::ConfigOverride>,
    child_args: Option<fcomponent::CreateChildArgs>,
) -> Result<(), CreateError> {
    let collection_ref = fdecl::CollectionRef { name: collection.to_string() };
    let decl = fdecl::Child {
        name: Some(child_name.to_string()),
        url: Some(url.to_string()),
        startup: Some(fdecl::StartupMode::Lazy),
        environment: None,
        config_overrides: Some(config_overrides),
        ..Default::default()
    };

    lifecycle_controller
        .create_instance(
            &parent.to_string(),
            &collection_ref,
            &decl,
            child_args.unwrap_or(fcomponent::CreateChildArgs::default()),
        )
        .await
        .map_err(|e| ActionError::Fidl(e))?
        .map_err(|e| match e {
            fsys::CreateError::BadChildDecl => CreateError::BadChildDecl,
            fsys::CreateError::CollectionNotFound => CreateError::CollectionNotFound,
            fsys::CreateError::InstanceAlreadyExists => CreateError::InstanceAlreadyExists,
            fsys::CreateError::Internal => ActionError::Internal.into(),
            fsys::CreateError::BadMoniker => ActionError::BadMoniker.into(),
            fsys::CreateError::InstanceNotFound => ActionError::InstanceNotFound.into(),
            _ => ActionError::UnknownError.into(),
        })?;

    Ok(())
}

/// Uses the `fuchsia.sys2.LifecycleController` protocol to destroy a dynamic component instance
/// with the given `moniker`.
pub async fn destroy_instance_in_collection(
    lifecycle_controller: &fsys::LifecycleControllerProxy,
    parent: &RelativeMoniker,
    collection: &str,
    child_name: &str,
) -> Result<(), DestroyError> {
    let child =
        fdecl::ChildRef { name: child_name.to_string(), collection: Some(collection.to_string()) };

    lifecycle_controller
        .destroy_instance(&parent.to_string(), &child)
        .await
        .map_err(|e| ActionError::Fidl(e))?
        .map_err(|e| match e {
            fsys::DestroyError::BadChildRef => DestroyError::BadChildRef,
            fsys::DestroyError::Internal => ActionError::Internal.into(),
            fsys::DestroyError::BadMoniker => ActionError::BadMoniker.into(),
            fsys::DestroyError::InstanceNotFound => ActionError::InstanceNotFound.into(),
            _ => ActionError::UnknownError.into(),
        })?;
    Ok(())
}

// A future that returns when the component instance has stopped.
// This notification comes over FIDL, which is why this future returns FIDL-specific errors.
type StopFuture = BoxFuture<'static, Result<(), fidl::Error>>;

/// Uses the `fuchsia.sys2.LifecycleController` protocol to start a component instance
/// with the given `moniker`.
///
/// Returns a future that can be waited on to know when the component instance has stopped.
pub async fn start_instance(
    lifecycle_controller: &fsys::LifecycleControllerProxy,
    moniker: &RelativeMoniker,
) -> Result<StopFuture, StartError> {
    let (client, server) = fidl::endpoints::create_proxy::<fcomponent::BinderMarker>().unwrap();
    lifecycle_controller
        .start_instance(&moniker.to_string(), server)
        .await
        .map_err(|e| ActionError::Fidl(e))?
        .map_err(|e| match e {
            fsys::StartError::PackageNotFound => StartError::PackageNotFound,
            fsys::StartError::ManifestNotFound => StartError::ManifestNotFound,
            fsys::StartError::Internal => ActionError::Internal.into(),
            fsys::StartError::BadMoniker => ActionError::BadMoniker.into(),
            fsys::StartError::InstanceNotFound => ActionError::InstanceNotFound.into(),
            _ => ActionError::UnknownError.into(),
        })?;
    let stop_future = async move {
        let mut event_stream = client.take_event_stream();
        match event_stream.next().await {
            Some(Err(e)) => return Err(e),
            None => return Ok(()),
            _ => unreachable!("The binder protocol does not have an event"),
        }
    }
    .boxed();
    Ok(stop_future)
}

/// Uses the `fuchsia.sys2.LifecycleController` protocol to stop a component instance
/// with the given `moniker`.
pub async fn stop_instance(
    lifecycle_controller: &fsys::LifecycleControllerProxy,
    moniker: &RelativeMoniker,
) -> Result<(), ActionError> {
    lifecycle_controller
        .stop_instance(&moniker.to_string())
        .await
        .map_err(|e| ActionError::Fidl(e))?
        .map_err(|e| match e {
            fsys::StopError::Internal => ActionError::Internal,
            fsys::StopError::BadMoniker => ActionError::BadMoniker,
            fsys::StopError::InstanceNotFound => ActionError::InstanceNotFound,
            _ => ActionError::UnknownError,
        })?;
    Ok(())
}

/// Uses the `fuchsia.sys2.LifecycleController` protocol to resolve a component instance
/// with the given `moniker`.
pub async fn resolve_instance(
    lifecycle_controller: &fsys::LifecycleControllerProxy,
    moniker: &RelativeMoniker,
) -> Result<(), ResolveError> {
    lifecycle_controller
        .resolve_instance(&moniker.to_string())
        .await
        .map_err(|e| ActionError::Fidl(e))?
        .map_err(|e| match e {
            fsys::ResolveError::PackageNotFound => ResolveError::PackageNotFound,
            fsys::ResolveError::ManifestNotFound => ResolveError::ManifestNotFound,
            fsys::ResolveError::Internal => ActionError::Internal.into(),
            fsys::ResolveError::BadMoniker => ActionError::BadMoniker.into(),
            fsys::ResolveError::InstanceNotFound => ActionError::InstanceNotFound.into(),
            _ => ActionError::UnknownError.into(),
        })?;
    Ok(())
}

/// Uses the `fuchsia.sys2.LifecycleController` protocol to unresolve a component instance
/// with the given `moniker`.
pub async fn unresolve_instance(
    lifecycle_controller: &fsys::LifecycleControllerProxy,
    moniker: &RelativeMoniker,
) -> Result<(), ActionError> {
    lifecycle_controller
        .unresolve_instance(&moniker.to_string())
        .await
        .map_err(|e| ActionError::Fidl(e))?
        .map_err(|e| match e {
            fsys::UnresolveError::Internal => ActionError::Internal,
            fsys::UnresolveError::BadMoniker => ActionError::BadMoniker,
            fsys::UnresolveError::InstanceNotFound => ActionError::InstanceNotFound,
            _ => ActionError::UnknownError,
        })?;
    Ok(())
}

#[cfg(test)]
mod test {
    use {
        super::*, assert_matches::assert_matches, fidl::endpoints::create_proxy_and_stream,
        fidl::HandleBased, fidl_fuchsia_process as fprocess, futures::TryStreamExt,
        moniker::RelativeMonikerBase,
    };

    fn lifecycle_create_instance(
        expected_moniker: &'static str,
        expected_collection: &'static str,
        expected_name: &'static str,
        expected_url: &'static str,
        expected_numbered_handle_count: usize,
    ) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::CreateInstance {
                    parent_moniker,
                    collection,
                    decl,
                    args,
                    responder,
                    ..
                } => {
                    assert_eq!(expected_moniker, parent_moniker);
                    assert_eq!(expected_collection, collection.name);
                    assert_eq!(expected_name, decl.name.unwrap());
                    assert_eq!(expected_url, decl.url.unwrap());
                    assert_eq!(
                        expected_numbered_handle_count,
                        args.numbered_handles.unwrap_or(vec![]).len()
                    );
                    responder.send(Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    fn lifecycle_destroy_instance(
        expected_moniker: &'static str,
        expected_collection: &'static str,
        expected_name: &'static str,
    ) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::DestroyInstance {
                    parent_moniker,
                    child,
                    responder,
                    ..
                } => {
                    assert_eq!(expected_moniker, parent_moniker);
                    assert_eq!(expected_name, child.name);
                    assert_eq!(expected_collection, child.collection.unwrap());
                    responder.send(Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    fn lifecycle_start(expected_moniker: &'static str) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::StartInstance { moniker, responder, .. } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    fn lifecycle_stop(expected_moniker: &'static str) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::StopInstance { moniker, responder, .. } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    fn lifecycle_resolve(expected_moniker: &'static str) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::ResolveInstance {
                    moniker, responder, ..
                } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    fn lifecycle_unresolve(expected_moniker: &'static str) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::UnresolveInstance {
                    moniker, responder, ..
                } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    fn lifecycle_create_fail(error: fsys::CreateError) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::CreateInstance { responder, .. } => {
                    responder.send(Err(error)).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    fn lifecycle_start_fail(error: fsys::StartError) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::StartInstance { responder, .. } => {
                    responder.send(Err(error)).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request"),
            }
        })
        .detach();
        lifecycle_controller
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_create_child() {
        let parent = RelativeMoniker::parse_str("./core").unwrap();
        let url =
            AbsoluteComponentUrl::parse("fuchsia-pkg://fuchsia.com/test#meta/test.cm").unwrap();
        let lc = lifecycle_create_instance(
            "./core",
            "foo",
            "bar",
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm",
            0,
        );
        create_instance_in_collection(&lc, &parent, "foo", "bar", &url, vec![], None)
            .await
            .unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_create_child_with_numbered_handles() {
        let parent = RelativeMoniker::parse_str("./core").unwrap();
        let url =
            AbsoluteComponentUrl::parse("fuchsia-pkg://fuchsia.com/test#meta/test.cm").unwrap();
        let lc = lifecycle_create_instance(
            "./core",
            "foo",
            "bar",
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm",
            2,
        );
        let (left, right) = fidl::Socket::create_stream();
        let child_args = fcomponent::CreateChildArgs {
            numbered_handles: Some(vec![
                fprocess::HandleInfo { handle: left.into_handle(), id: 0x10 },
                fprocess::HandleInfo { handle: right.into_handle(), id: 0x11 },
            ]),
            ..Default::default()
        };
        create_instance_in_collection(&lc, &parent, "foo", "bar", &url, vec![], Some(child_args))
            .await
            .unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_create_already_exists() {
        let parent = RelativeMoniker::parse_str("./core").unwrap();
        let url =
            AbsoluteComponentUrl::parse("fuchsia-pkg://fuchsia.com/test#meta/test.cm").unwrap();
        let lc = lifecycle_create_fail(fsys::CreateError::InstanceAlreadyExists);
        let err = create_instance_in_collection(&lc, &parent, "foo", "bar", &url, vec![], None)
            .await
            .unwrap_err();
        assert_matches!(err, CreateError::InstanceAlreadyExists);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_destroy_child() {
        let parent = RelativeMoniker::parse_str("./core").unwrap();
        let lc = lifecycle_destroy_instance("./core", "foo", "bar");
        destroy_instance_in_collection(&lc, &parent, "foo", "bar").await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start() {
        let moniker = RelativeMoniker::parse_str("./core/foo").unwrap();
        let lc = lifecycle_start("./core/foo");
        start_instance(&lc, &moniker).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop() {
        let moniker = RelativeMoniker::parse_str("./core/foo").unwrap();
        let lc = lifecycle_stop("./core/foo");
        stop_instance(&lc, &moniker).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_resolve() {
        let moniker = RelativeMoniker::parse_str("./core/foo").unwrap();
        let lc = lifecycle_resolve("./core/foo");
        resolve_instance(&lc, &moniker).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_unresolve() {
        let moniker = RelativeMoniker::parse_str("./core/foo").unwrap();
        let lc = lifecycle_unresolve("./core/foo");
        unresolve_instance(&lc, &moniker).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_instance_not_found() {
        let moniker = RelativeMoniker::parse_str("./core/foo").unwrap();
        let lc = lifecycle_start_fail(fsys::StartError::InstanceNotFound);
        match start_instance(&lc, &moniker).await {
            Ok(_) => panic!("start shouldn't succeed"),
            Err(StartError::ActionError(ActionError::InstanceNotFound)) => {}
            Err(e) => panic!("start failed unexpectedly: {}", e),
        }
    }
}
