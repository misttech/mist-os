// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tools for starting or connecting to existing Fuchsia applications and services.

use anyhow::{format_err, Context as _, Error};
use fidl::endpoints::{
    DiscoverableProtocolMarker, MemberOpener, ProtocolMarker, ServiceMarker, ServiceProxy,
};
use fidl_fuchsia_component::{RealmMarker, RealmProxy};
use fidl_fuchsia_component_decl::ChildRef;
use fidl_fuchsia_io as fio;
use fuchsia_fs::directory::{WatchEvent, Watcher};
use futures::stream::FusedStream;
use futures::{Stream, StreamExt};
use pin_project::pin_project;
use std::borrow::Borrow;
use std::marker::PhantomData;
use std::pin::pin;
use std::task::Poll;

use crate::directory::{open_directory_async, AsRefDirectory};
use crate::SVC_DIR;

/// A protocol connection request that allows checking if the protocol exists.
pub struct ProtocolConnector<D: Borrow<fio::DirectoryProxy>, P: DiscoverableProtocolMarker> {
    svc_dir: D,
    _svc_marker: PhantomData<P>,
}

impl<D: Borrow<fio::DirectoryProxy>, P: DiscoverableProtocolMarker> ProtocolConnector<D, P> {
    /// Returns a new `ProtocolConnector` to `P` in the specified service directory.
    fn new(svc_dir: D) -> ProtocolConnector<D, P> {
        ProtocolConnector { svc_dir, _svc_marker: PhantomData }
    }

    /// Returns `true` if the protocol exists in the service directory.
    ///
    /// This method requires a round trip to the service directory to check for
    /// existence.
    pub async fn exists(&self) -> Result<bool, Error> {
        match fuchsia_fs::directory::dir_contains(self.svc_dir.borrow(), P::PROTOCOL_NAME).await {
            Ok(v) => Ok(v),
            // If the service directory is unavailable, then mask the error as if
            // the protocol does not exist.
            Err(fuchsia_fs::directory::EnumerateError::Fidl(
                _,
                fidl::Error::ClientChannelClosed { status, protocol_name: _ },
            )) if status == zx::Status::PEER_CLOSED => Ok(false),
            Err(e) => Err(Error::new(e).context("error checking for service entry in directory")),
        }
    }

    /// Connect to the FIDL protocol using the provided server-end.
    ///
    /// Note, this method does not check if the protocol exists. It is up to the
    /// caller to call `exists` to check for existence.
    pub fn connect_with(self, server_end: zx::Channel) -> Result<(), Error> {
        self.svc_dir
            .borrow()
            .open3(
                P::PROTOCOL_NAME,
                fio::Flags::PROTOCOL_SERVICE,
                &fio::Options::default(),
                server_end.into(),
            )
            .context("error connecting to protocol")
    }

    /// Connect to the FIDL protocol.
    ///
    /// Note, this method does not check if the protocol exists. It is up to the
    /// caller to call `exists` to check for existence.
    pub fn connect(self) -> Result<P::Proxy, Error> {
        let (proxy, server_end) = fidl::endpoints::create_proxy::<P>();
        let () = self
            .connect_with(server_end.into_channel())
            .context("error connecting with server channel")?;
        Ok(proxy)
    }
}

/// Clone the handle to the service directory in the application's root namespace.
pub fn clone_namespace_svc() -> Result<fio::DirectoryProxy, Error> {
    fuchsia_fs::directory::open_in_namespace(SVC_DIR, fio::Flags::empty())
        .context("error opening svc directory")
}

/// Return a FIDL protocol connector at the default service directory in the
/// application's root namespace.
pub fn new_protocol_connector<P: DiscoverableProtocolMarker>(
) -> Result<ProtocolConnector<fio::DirectoryProxy, P>, Error> {
    new_protocol_connector_at::<P>(SVC_DIR)
}

/// Return a FIDL protocol connector at the specified service directory in the
/// application's root namespace.
///
/// The service directory path must be an absolute path.
pub fn new_protocol_connector_at<P: DiscoverableProtocolMarker>(
    service_directory_path: &str,
) -> Result<ProtocolConnector<fio::DirectoryProxy, P>, Error> {
    let dir = fuchsia_fs::directory::open_in_namespace(service_directory_path, fio::Flags::empty())
        .context("error opening service directory")?;

    Ok(ProtocolConnector::new(dir))
}

/// Return a FIDL protocol connector at the specified service directory.
pub fn new_protocol_connector_in_dir<P: DiscoverableProtocolMarker>(
    dir: &fio::DirectoryProxy,
) -> ProtocolConnector<&fio::DirectoryProxy, P> {
    ProtocolConnector::new(dir)
}

/// Connect to a FIDL protocol using the provided channel.
pub fn connect_channel_to_protocol<P: DiscoverableProtocolMarker>(
    server_end: zx::Channel,
) -> Result<(), Error> {
    connect_channel_to_protocol_at::<P>(server_end, SVC_DIR)
}

/// Connect to a FIDL protocol using the provided channel and namespace prefix.
pub fn connect_channel_to_protocol_at<P: DiscoverableProtocolMarker>(
    server_end: zx::Channel,
    service_directory_path: &str,
) -> Result<(), Error> {
    let protocol_path = format!("{}/{}", service_directory_path, P::PROTOCOL_NAME);
    connect_channel_to_protocol_at_path(server_end, &protocol_path)
}

/// Connect to a FIDL protocol using the provided channel and namespace path.
pub fn connect_channel_to_protocol_at_path(
    server_end: zx::Channel,
    protocol_path: &str,
) -> Result<(), Error> {
    fdio::service_connect(&protocol_path, server_end)
        .with_context(|| format!("Error connecting to protocol path: {}", protocol_path))
}

/// Connect to a FIDL protocol using the application root namespace.
pub fn connect_to_protocol<P: DiscoverableProtocolMarker>() -> Result<P::Proxy, Error> {
    connect_to_protocol_at::<P>(SVC_DIR)
}

/// Connect to a FIDL protocol using the application root namespace, returning a synchronous proxy.
///
/// Note: while this function returns a synchronous thread-blocking proxy it does not block until
/// the connection is complete. The proxy must be used to discover whether the connection was
/// successful.
pub fn connect_to_protocol_sync<P: DiscoverableProtocolMarker>(
) -> Result<P::SynchronousProxy, Error> {
    connect_to_protocol_sync_at::<P>(SVC_DIR)
}

/// Connect to a FIDL protocol using the provided namespace prefix.
pub fn connect_to_protocol_at<P: DiscoverableProtocolMarker>(
    service_prefix: impl AsRef<str>,
) -> Result<P::Proxy, Error> {
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P>();
    let () =
        connect_channel_to_protocol_at::<P>(server_end.into_channel(), service_prefix.as_ref())?;
    Ok(proxy)
}

/// Connect to a FIDL protocol using the provided namespace prefix, returning a synchronous proxy.
///
/// Note: while this function returns a synchronous thread-blocking proxy it does not block until
/// the connection is complete. The proxy must be used to discover whether the connection was
/// successful.
pub fn connect_to_protocol_sync_at<P: DiscoverableProtocolMarker>(
    service_prefix: impl AsRef<str>,
) -> Result<P::SynchronousProxy, Error> {
    let (proxy, server_end) = fidl::endpoints::create_sync_proxy::<P>();
    let () =
        connect_channel_to_protocol_at::<P>(server_end.into_channel(), service_prefix.as_ref())?;
    Ok(proxy)
}

/// Connect to a FIDL protocol using the provided path.
pub fn connect_to_protocol_at_path<P: ProtocolMarker>(
    protocol_path: impl AsRef<str>,
) -> Result<P::Proxy, Error> {
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P>();
    let () =
        connect_channel_to_protocol_at_path(server_end.into_channel(), protocol_path.as_ref())?;
    Ok(proxy)
}

/// Connect to an instance of a FIDL protocol hosted in `directory`.
pub fn connect_to_protocol_at_dir_root<P: DiscoverableProtocolMarker>(
    directory: &impl AsRefDirectory,
) -> Result<P::Proxy, Error> {
    connect_to_named_protocol_at_dir_root::<P>(directory, P::PROTOCOL_NAME)
}

/// Connect to an instance of a FIDL protocol hosted in `directory` using the given `filename`.
pub fn connect_to_named_protocol_at_dir_root<P: ProtocolMarker>(
    directory: &impl AsRefDirectory,
    filename: &str,
) -> Result<P::Proxy, Error> {
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P>();
    directory.as_ref_directory().open(
        filename,
        fio::Flags::PROTOCOL_SERVICE,
        server_end.into_channel().into(),
    )?;
    Ok(proxy)
}

/// Connect to an instance of a FIDL protocol hosted in `directory`, in the `/svc/` subdir.
pub fn connect_to_protocol_at_dir_svc<P: DiscoverableProtocolMarker>(
    directory: &impl AsRefDirectory,
) -> Result<P::Proxy, Error> {
    let protocol_path = format!("{}/{}", SVC_DIR, P::PROTOCOL_NAME);
    connect_to_named_protocol_at_dir_root::<P>(directory, &protocol_path)
}

/// This wraps an instance directory for a service capability and provides the MemberOpener trait
/// for it. This can be boxed and used with a |ServiceProxy::from_member_opener|.
pub struct ServiceInstanceDirectory(pub fio::DirectoryProxy);

impl MemberOpener for ServiceInstanceDirectory {
    fn open_member(&self, member: &str, server_end: zx::Channel) -> Result<(), fidl::Error> {
        let Self(directory) = self;
        directory.open3(member, fio::Flags::PROTOCOL_SERVICE, &fio::Options::default(), server_end)
    }
}

/// An instance of an aggregated fidl service that has been enumerated by [`ServiceWatcher::watch`]
/// or [`ServiceWatcher::watch_for_any`].
pub struct ServiceInstance<'a, S> {
    /// The name of the service instance within the service directory
    pub name: String,
    service: &'a Service<S>,
}

// note: complicated bounds here can help the compiler deduce the service marker from a known
// proxy type.
impl<'a, P: ServiceProxy, S: ServiceMarker<Proxy = P>> ServiceInstance<'a, S> {
    /// Connects to the instance named by [`Self::name`] in the service directory that enumerated
    /// it.
    pub async fn connect(&self) -> Result<P, Error> {
        self.service.connect_to_instance(&self.name)
    }
}

/// A service from an incoming namespace's `/svc` directory.
pub struct Service<S> {
    dir: fio::DirectoryProxy,
    _marker: S,
}

/// Returns a new [`Service`] that waits for instances to appear in
/// the given service directory, probably opened by [`open_service`]
impl<S> From<fio::DirectoryProxy> for Service<S>
where
    S: Default,
{
    fn from(dir: fio::DirectoryProxy) -> Self {
        Self { dir, _marker: S::default() }
    }
}

impl<S: ServiceMarker> Service<S> {
    /// Returns a new [`Service`] that waits for instances to appear in
    /// the given service directory, probably opened by [`open_service`]
    pub fn from_service_dir_proxy(dir: fio::DirectoryProxy, _marker: S) -> Self {
        Self { dir, _marker }
    }

    /// Returns a new [`Service`] from the process's incoming service namespace.
    pub fn open(marker: S) -> Result<Self, Error> {
        Ok(Self::from_service_dir_proxy(open_service::<S>()?, marker))
    }

    /// Returns a new [`Service`] that is in the given directory.
    pub fn open_from_dir(svc_dir: impl AsRefDirectory, marker: S) -> Result<Self, Error> {
        let dir = open_directory_async(&svc_dir, S::SERVICE_NAME, fio::Rights::empty())?;
        Ok(Self::from_service_dir_proxy(dir, marker))
    }

    /// Returns a new [`Service`] that is in the given directory under the given prefix
    /// (as "{prefix}/ServiceName"). A common case would be passing [`SVC_DIR`] as the prefix.
    pub fn open_from_dir_prefix(
        dir: impl AsRefDirectory,
        prefix: impl AsRef<str>,
        marker: S,
    ) -> Result<Self, Error> {
        let prefix = prefix.as_ref();
        let service_path = format!("{prefix}/{}", S::SERVICE_NAME);
        // TODO(https://fxbug.dev/42068248): Some Directory implementations require relative paths,
        // even though they aren't technically supposed to, so strip the leading slash until that's
        // resolved one way or the other.
        let service_path = service_path.strip_prefix('/').unwrap_or_else(|| service_path.as_ref());
        let dir = open_directory_async(&dir, &service_path, fio::Rights::empty())?;
        Ok(Self::from_service_dir_proxy(dir, marker))
    }

    /// Connects to the named instance without waiting for it to appear. You should only use this
    /// after the instance name has been returned by the [`Self::watch`] stream, or if the
    /// instance is statically routed so component manager will lazily load it.
    pub fn connect_to_instance(&self, name: impl AsRef<str>) -> Result<S::Proxy, Error> {
        connect_to_instance_in_service_dir::<S>(&self.dir, name.as_ref())
    }

    /// Returns an async stream of service instances that are enumerated within this service
    /// directory.
    pub async fn watch(&self) -> Result<ServiceInstanceStream<'_, S>, Error> {
        let watcher = Watcher::new(&self.dir).await?;
        let finished = false;
        Ok(ServiceInstanceStream { service: self, watcher, finished })
    }

    /// Asynchronously returns the first service instance available within this service directory.
    pub async fn watch_for_any(&self) -> Result<ServiceInstance<'_, S>, Error> {
        self.watch()
            .await?
            .next()
            .await
            .context("No instances found before service directory was removed")?
    }
}

/// A stream iterator for a service directory that produces one item for every service instance
/// that is added to it as they are added. Returned from [`Service::watch`]
///
/// Normally, this stream will only terminate if the service directory being watched is removed, so
/// the client must decide when it has found all the instances it's looking for.
#[pin_project]
pub struct ServiceInstanceStream<'a, S> {
    service: &'a Service<S>,
    watcher: Watcher,
    finished: bool,
}
impl<'a, S> Stream for ServiceInstanceStream<'a, S> {
    type Item = Result<ServiceInstance<'a, S>, Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.project();
        use Poll::*;
        if *this.finished {
            return Poll::Ready(None);
        }
        // poll the inner watcher until we either find something worth returning or it
        // returns Pending.
        while let Ready(next) = this.watcher.poll_next_unpin(cx) {
            match next {
                Some(Ok(state)) => match state.event {
                    WatchEvent::DELETED => {
                        *this.finished = true;
                        return Ready(None);
                    }
                    WatchEvent::ADD_FILE | WatchEvent::EXISTING => {
                        let filename = state.filename.to_str().unwrap();
                        if filename != "." {
                            return Ready(Some(Ok(ServiceInstance {
                                service: this.service,
                                name: filename.to_owned(),
                            })));
                        }
                    }
                    _ => {}
                },
                Some(Err(err)) => {
                    *this.finished = true;
                    return Ready(Some(Err(err.into())));
                }
                None => {
                    *this.finished = true;
                    return Ready(None);
                }
            }
        }
        Pending
    }
}

impl<'a, S> FusedStream for ServiceInstanceStream<'a, S> {
    fn is_terminated(&self) -> bool {
        self.finished
    }
}

/// Connect to an instance of a FIDL service in the `/svc` directory of
/// the application's root namespace.
/// `instance` is a path of one or more components.
// NOTE: We would like to use impl AsRef<T> to accept a wide variety of string-like
// inputs but Rust limits specifying explicit generic parameters when `impl-traits`
// are present.
pub fn connect_to_service_instance<S: ServiceMarker>(instance: &str) -> Result<S::Proxy, Error> {
    connect_to_service_instance_at::<S>(SVC_DIR, instance)
}

/// Connect to an instance of a FIDL service using the provided path prefix.
/// `path_prefix` should not contain any slashes.
/// `instance` is a path of one or more components.
// NOTE: We would like to use impl AsRef<T> to accept a wide variety of string-like
// inputs but Rust limits specifying explicit generic parameters when `impl-traits`
// are present.
pub fn connect_to_service_instance_at<S: ServiceMarker>(
    path_prefix: &str,
    instance: &str,
) -> Result<S::Proxy, Error> {
    let service_path = format!("{}/{}/{}", path_prefix, S::SERVICE_NAME, instance);
    let directory_proxy =
        fuchsia_fs::directory::open_in_namespace(&service_path, fio::Flags::empty())?;
    Ok(S::Proxy::from_member_opener(Box::new(ServiceInstanceDirectory(directory_proxy))))
}

/// Connect to an instance of a FIDL service hosted on the directory protocol channel `directory`.
/// `instance` is a path of one or more components.
pub fn connect_to_instance_in_service_dir<S: ServiceMarker>(
    directory: &fio::DirectoryProxy,
    instance: &str,
) -> Result<S::Proxy, Error> {
    let directory_proxy =
        fuchsia_fs::directory::open_directory_async(directory, instance, fio::Flags::empty())?;
    Ok(S::Proxy::from_member_opener(Box::new(ServiceInstanceDirectory(directory_proxy))))
}

/// Connect to a named instance of a FIDL service hosted in the service subdirectory under the
/// directory protocol channel `directory`
// NOTE: We would like to use impl AsRef<T> to accept a wide variety of string-like
// inputs but Rust limits specifying explicit generic parameters when `impl-traits`
// are present.
pub fn connect_to_service_instance_at_dir<S: ServiceMarker>(
    directory: &fio::DirectoryProxy,
    instance: &str,
) -> Result<S::Proxy, Error> {
    let service_path = format!("{}/{}", S::SERVICE_NAME, instance);
    let directory_proxy =
        fuchsia_fs::directory::open_directory_async(directory, &service_path, fio::Flags::empty())?;
    Ok(S::Proxy::from_member_opener(Box::new(ServiceInstanceDirectory(directory_proxy))))
}

/// Connect to an instance of a FIDL service hosted in `directory`, in the `svc/` subdir.
pub fn connect_to_service_instance_at_dir_svc<S: ServiceMarker>(
    directory: &impl AsRefDirectory,
    instance: impl AsRef<str>,
) -> Result<S::Proxy, Error> {
    let service_path = format!("{SVC_DIR}/{}/{}", S::SERVICE_NAME, instance.as_ref());
    // TODO(https://fxbug.dev/42068248): Some Directory implementations require relative paths,
    // even though they aren't technically supposed to, so strip the leading slash until that's
    // resolved one way or the other.
    let service_path = service_path.strip_prefix('/').unwrap();
    let directory_proxy = open_directory_async(directory, service_path, fio::Rights::empty())?;
    Ok(S::Proxy::from_member_opener(Box::new(ServiceInstanceDirectory(directory_proxy))))
}

/// Opens a FIDL service as a directory, which holds instances of the service.
pub fn open_service<S: ServiceMarker>() -> Result<fio::DirectoryProxy, Error> {
    let service_path = format!("{}/{}", SVC_DIR, S::SERVICE_NAME);
    let flags = fio::PERM_READABLE;
    fuchsia_fs::directory::open_in_namespace(&service_path, flags).context("namespace open failed")
}

/// Opens a FIDL service hosted in `directory` as a directory, which holds
/// instances of the service.
pub fn open_service_at_dir<S: ServiceMarker>(
    directory: &fio::DirectoryProxy,
) -> Result<fio::DirectoryProxy, Error> {
    let flags = fio::PERM_READABLE;
    fuchsia_fs::directory::open_directory_async(directory, S::SERVICE_NAME, flags)
        .map_err(Into::into)
}

/// Opens the exposed directory from a child. Only works in CFv2, and only works if this component
/// uses `fuchsia.component.Realm`.
pub async fn open_childs_exposed_directory(
    child_name: impl Into<String>,
    collection_name: Option<String>,
) -> Result<fio::DirectoryProxy, Error> {
    let realm_proxy = connect_to_protocol::<RealmMarker>()?;
    let (directory_proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let child_ref = ChildRef { name: child_name.into(), collection: collection_name };
    realm_proxy.open_exposed_dir(&child_ref, server_end).await?.map_err(|e| {
        let ChildRef { name, collection } = child_ref;
        format_err!("failed to bind to child {} in collection {:?}: {:?}", name, collection, e)
    })?;
    Ok(directory_proxy)
}

/// Connects to a FIDL protocol exposed by a child that's within the `/svc` directory. Only works in
/// CFv2, and only works if this component uses `fuchsia.component.Realm`.
pub async fn connect_to_childs_protocol<P: DiscoverableProtocolMarker>(
    child_name: String,
    collection_name: Option<String>,
) -> Result<P::Proxy, Error> {
    let child_exposed_directory =
        open_childs_exposed_directory(child_name, collection_name).await?;
    connect_to_protocol_at_dir_root::<P>(&child_exposed_directory)
}

/// Returns a connection to the Realm protocol. Components v2 only.
pub fn realm() -> Result<RealmProxy, Error> {
    connect_to_protocol::<RealmMarker>()
}

#[allow(missing_docs)]
#[cfg(test)]
pub mod test_util {
    use super::*;
    use std::sync::Arc;
    use vfs::directory::entry_container::Directory;
    use vfs::execution_scope::ExecutionScope;
    use vfs::ObjectRequest;

    #[cfg(test)]
    pub fn run_directory_server(dir: Arc<dyn Directory>) -> fio::DirectoryProxy {
        let (dir_proxy, dir_server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        let scope = ExecutionScope::new();
        let flags =
            fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::from_bits(fio::R_STAR_DIR.bits()).unwrap();
        ObjectRequest::new3(flags, &fio::Options::default(), dir_server.into_channel())
            .handle(|request| dir.open3(scope, vfs::path::Path::dot(), flags, request));
        dir_proxy
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;
    use fidl::endpoints::ServiceMarker as _;
    use fidl_fuchsia_component_client_test::{
        ProtocolAMarker, ProtocolAProxy, ProtocolBMarker, ProtocolBProxy, ServiceMarker,
    };
    use fuchsia_async::{self as fasync};
    use futures::{future, TryStreamExt};
    use vfs::directory::simple::Simple;
    use vfs::file::vmo::read_only;
    use vfs::pseudo_directory;

    #[fasync::run_singlethreaded(test)]
    async fn test_svc_connector_svc_does_not_exist() -> Result<(), Error> {
        let req = new_protocol_connector::<ProtocolAMarker>().context("error probing service")?;
        assert_matches::assert_matches!(
            req.exists().await.context("error checking service"),
            Ok(false)
        );
        let _: ProtocolAProxy = req.connect().context("error connecting to service")?;

        let req = new_protocol_connector_at::<ProtocolAMarker>(SVC_DIR)
            .context("error probing service at svc dir")?;
        assert_matches::assert_matches!(
            req.exists().await.context("error checking service at svc dir"),
            Ok(false)
        );
        let _: ProtocolAProxy = req.connect().context("error connecting to service at svc dir")?;

        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_svc_connector_connect_with_dir() -> Result<(), Error> {
        let dir = pseudo_directory! {
            ProtocolBMarker::PROTOCOL_NAME => read_only("read_only"),
        };
        let dir_proxy = test_util::run_directory_server(dir);
        let req = new_protocol_connector_in_dir::<ProtocolAMarker>(&dir_proxy);
        assert_matches::assert_matches!(
            req.exists().await.context("error probing invalid service"),
            Ok(false)
        );
        let _: ProtocolAProxy = req.connect().context("error connecting to invalid service")?;

        let req = new_protocol_connector_in_dir::<ProtocolBMarker>(&dir_proxy);
        assert_matches::assert_matches!(
            req.exists().await.context("error probing service"),
            Ok(true)
        );
        let _: ProtocolBProxy = req.connect().context("error connecting to service")?;

        Ok(())
    }

    fn make_inner_service_instance_tree() -> Arc<Simple> {
        pseudo_directory! {
            ServiceMarker::SERVICE_NAME => pseudo_directory! {
                "default" => read_only("read_only"),
                "another_instance" => read_only("read_only"),
            },
        }
    }

    fn make_service_instance_tree() -> Arc<Simple> {
        pseudo_directory! {
            "svc" => make_inner_service_instance_tree(),
        }
    }

    #[fasync::run_until_stalled(test)]
    async fn test_service_instance_watcher_from_root() -> Result<(), Error> {
        let dir_proxy = test_util::run_directory_server(make_service_instance_tree());
        let watcher = Service::open_from_dir_prefix(&dir_proxy, SVC_DIR, ServiceMarker)?;
        let found_names: HashSet<_> = watcher
            .watch()
            .await?
            .take(2)
            .and_then(|service| future::ready(Ok(service.name)))
            .try_collect()
            .await?;
        assert_eq!(
            found_names,
            HashSet::from_iter(["default".to_owned(), "another_instance".to_owned()])
        );

        Ok(())
    }

    #[fasync::run_until_stalled(test)]
    async fn test_service_instance_watcher_from_svc() -> Result<(), Error> {
        let dir_proxy = test_util::run_directory_server(make_inner_service_instance_tree());
        let watcher = Service::open_from_dir(&dir_proxy, ServiceMarker)?;
        let found_names: HashSet<_> = watcher
            .watch()
            .await?
            .take(2)
            .and_then(|service| future::ready(Ok(service.name)))
            .try_collect()
            .await?;
        assert_eq!(
            found_names,
            HashSet::from_iter(["default".to_owned(), "another_instance".to_owned()])
        );

        Ok(())
    }

    #[fasync::run_until_stalled(test)]
    async fn test_connect_to_all_services() -> Result<(), Error> {
        let dir_proxy = test_util::run_directory_server(make_service_instance_tree());
        let watcher = Service::open_from_dir_prefix(&dir_proxy, SVC_DIR, ServiceMarker)?;
        let _: Vec<_> = watcher
            .watch()
            .await?
            .take(2)
            .and_then(|service| async move { service.connect().await })
            .try_collect()
            .await?;

        Ok(())
    }

    #[fasync::run_until_stalled(test)]
    async fn test_connect_to_any() -> Result<(), Error> {
        let dir_proxy = test_util::run_directory_server(make_service_instance_tree());
        let watcher = Service::open_from_dir_prefix(&dir_proxy, SVC_DIR, ServiceMarker)?;
        let found = watcher.watch_for_any().await?;
        assert!(["default", "another_instance"].contains(&&*found.name));
        found.connect().await?;

        Ok(())
    }
}
