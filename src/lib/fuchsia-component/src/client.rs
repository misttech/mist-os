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
use std::borrow::Borrow;
use std::marker::PhantomData;
use {fidl_fuchsia_io as fio, zx};

use crate::directory::AsRefDirectory;
use crate::DEFAULT_SERVICE_INSTANCE;

/// Path to the service directory in an application's root namespace.
const SVC_DIR: &'static str = "/svc";

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
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<P>().context("error creating proxy")?;
        let () = self
            .connect_with(server_end.into_channel())
            .context("error connecting with server channel")?;
        Ok(proxy)
    }
}

/// Clone the handle to the service directory in the application's root namespace.
pub fn clone_namespace_svc() -> Result<fio::DirectoryProxy, Error> {
    fuchsia_fs::directory::open_in_namespace_deprecated(SVC_DIR, fio::OpenFlags::empty())
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
    let dir = fuchsia_fs::directory::open_in_namespace_deprecated(
        service_directory_path,
        fio::OpenFlags::empty(),
    )
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
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P>()?;
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
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P>()?;
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
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P>()?;
    directory.as_ref_directory().open(
        filename,
        fio::Flags::PROTOCOL_SERVICE,
        server_end.into_channel().into(),
    )?;
    Ok(proxy)
}

/// Connect to an instance of a FIDL protocol hosted in `directory`, in the `svc/` subdir.
pub fn connect_to_protocol_at_dir_svc<P: DiscoverableProtocolMarker>(
    directory: &impl AsRefDirectory,
) -> Result<P::Proxy, Error> {
    let protocol_path = format!("{}/{}", SVC_DIR, P::PROTOCOL_NAME);
    // TODO(https://fxbug.dev/42068248): Remove the following line when component
    // manager no longer mishandles leading slashes.
    let protocol_path = protocol_path.strip_prefix('/').unwrap();
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

/// Connect to the "default" instance of a FIDL service in the `/svc` directory of
/// the application's root namespace.
pub fn connect_to_service<S: ServiceMarker>() -> Result<S::Proxy, Error> {
    connect_to_service_instance_at::<S>(SVC_DIR, DEFAULT_SERVICE_INSTANCE)
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
    let directory_proxy = fuchsia_fs::directory::open_in_namespace_deprecated(
        &service_path,
        fio::OpenFlags::empty(),
    )?;
    Ok(S::Proxy::from_member_opener(Box::new(ServiceInstanceDirectory(directory_proxy))))
}

/// Connect to the "default" instance of a FIDL service hosted on the directory protocol
/// channel `directory`.
pub fn connect_to_default_instance_in_service_dir<S: ServiceMarker>(
    directory: &fio::DirectoryProxy,
) -> Result<S::Proxy, Error> {
    connect_to_instance_in_service_dir::<S>(directory, DEFAULT_SERVICE_INSTANCE)
}

/// Connect to an instance of a FIDL service hosted on the directory protocol channel `directory`.
/// `instance` is a path of one or more components.
pub fn connect_to_instance_in_service_dir<S: ServiceMarker>(
    directory: &fio::DirectoryProxy,
    instance: &str,
) -> Result<S::Proxy, Error> {
    let directory_proxy = fuchsia_fs::directory::open_directory_no_describe_deprecated(
        directory,
        &instance.to_string(),
        fio::OpenFlags::empty(),
    )?;
    Ok(S::Proxy::from_member_opener(Box::new(ServiceInstanceDirectory(directory_proxy))))
}

/// Connect to the "default" instance of a FIDL service hosted in the service subdirectory under
/// the directory protocol channel `directory`
pub fn connect_to_service_at_dir<S: ServiceMarker>(
    directory: &fio::DirectoryProxy,
) -> Result<S::Proxy, Error> {
    connect_to_service_instance_at_dir::<S>(directory, DEFAULT_SERVICE_INSTANCE)
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
    let directory_proxy = fuchsia_fs::directory::open_directory_no_describe_deprecated(
        directory,
        &service_path,
        fio::OpenFlags::empty(),
    )?;
    Ok(S::Proxy::from_member_opener(Box::new(ServiceInstanceDirectory(directory_proxy))))
}

/// Connect to the "default" instance of a FIDL service hosted in `directory`.
pub fn connect_to_service_at_channel<S: ServiceMarker>(
    directory: &zx::Channel,
) -> Result<S::Proxy, Error> {
    connect_to_service_instance_at_channel::<S>(directory, DEFAULT_SERVICE_INSTANCE)
}

/// Connect to an instance of a FIDL service hosted in `directory`.
/// `instance` is a path of one or more components.
// NOTE: We would like to use impl AsRef<T> to accept a wide variety of string-like
// inputs but Rust limits specifying explicit generic parameters when `impl-traits`
// are present.
pub fn connect_to_service_instance_at_channel<S: ServiceMarker>(
    directory: &zx::Channel,
    instance: &str,
) -> Result<S::Proxy, Error> {
    let service_path = format!("{}/{}", S::SERVICE_NAME, instance);
    let (directory_proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;
    // NB: This has to use `fdio` because we are holding a channel rather than a
    // proxy, and we can't make FIDL calls on unowned channels in Rust.
    let () = fdio::open_at(
        directory,
        &service_path,
        fio::OpenFlags::DIRECTORY,
        server_end.into_channel(),
    )?;
    Ok(S::Proxy::from_member_opener(Box::new(ServiceInstanceDirectory(directory_proxy))))
}

/// Opens a FIDL service as a directory, which holds instances of the service.
pub fn open_service<S: ServiceMarker>() -> Result<fio::DirectoryProxy, Error> {
    let service_path = format!("{}/{}", SVC_DIR, S::SERVICE_NAME);
    fuchsia_fs::directory::open_in_namespace_deprecated(&service_path, fio::OpenFlags::empty())
        .context("namespace open failed")
}

/// Opens a FIDL service hosted in `directory` as a directory, which holds
/// instances of the service.
pub fn open_service_at_dir<S: ServiceMarker>(
    directory: &fio::DirectoryProxy,
) -> Result<fio::DirectoryProxy, Error> {
    fuchsia_fs::directory::open_directory_no_describe_deprecated(
        directory,
        S::SERVICE_NAME,
        fio::OpenFlags::empty(),
    )
    .map_err(Into::into)
}

/// Opens the exposed directory from a child. Only works in CFv2, and only works if this component
/// uses `fuchsia.component.Realm`.
pub async fn open_childs_exposed_directory(
    child_name: impl Into<String>,
    collection_name: Option<String>,
) -> Result<fio::DirectoryProxy, Error> {
    let realm_proxy = connect_to_protocol::<RealmMarker>()?;
    let (directory_proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;
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
        let (dir_proxy, dir_server) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
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
    use super::*;
    use fidl_fuchsia_component_client_test::{
        ServiceAMarker, ServiceAProxy, ServiceBMarker, ServiceBProxy,
    };
    use fuchsia_async as fasync;
    use vfs::file::vmo::read_only;
    use vfs::pseudo_directory;

    #[fasync::run_singlethreaded(test)]
    async fn test_svc_connector_svc_does_not_exist() -> Result<(), Error> {
        let req = new_protocol_connector::<ServiceAMarker>().context("error probing service")?;
        assert_matches::assert_matches!(
            req.exists().await.context("error checking service"),
            Ok(false)
        );
        let _: ServiceAProxy = req.connect().context("error connecting to service")?;

        let req = new_protocol_connector_at::<ServiceAMarker>(SVC_DIR)
            .context("error probing service at svc dir")?;
        assert_matches::assert_matches!(
            req.exists().await.context("error checking service at svc dir"),
            Ok(false)
        );
        let _: ServiceAProxy = req.connect().context("error connecting to service at svc dir")?;

        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_svc_connector_connect_with_dir() -> Result<(), Error> {
        let dir = pseudo_directory! {
            ServiceBMarker::PROTOCOL_NAME => read_only("read_only"),
        };
        let dir_proxy = test_util::run_directory_server(dir);
        let req = new_protocol_connector_in_dir::<ServiceAMarker>(&dir_proxy);
        assert_matches::assert_matches!(
            req.exists().await.context("error probing invalid service"),
            Ok(false)
        );
        let _: ServiceAProxy = req.connect().context("error connecting to invalid service")?;

        let req = new_protocol_connector_in_dir::<ServiceBMarker>(&dir_proxy);
        assert_matches::assert_matches!(
            req.exists().await.context("error probing service"),
            Ok(true)
        );
        let _: ServiceBProxy = req.connect().context("error connecting to service")?;

        Ok(())
    }
}
