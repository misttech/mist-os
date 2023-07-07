// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, Context as _},
    fidl_fuchsia_boot as fboot, fidl_fuchsia_component_decl as fcomponent_decl,
    fidl_fuchsia_component_resolution as fcomponent_resolution, fidl_fuchsia_io as fio,
    fidl_fuchsia_pkg as fpkg,
    futures::{future::TryFutureExt as _, stream::StreamExt as _},
    std::collections::HashMap,
    tracing::error,
};

mod component;
mod context_authenticator;
mod package;

#[fuchsia::main]
async fn main() -> anyhow::Result<()> {
    tracing::info!("started");

    let blobfs = blobfs::Client::open_from_namespace_rx().context("failed to open /blob")?;
    let base_packages = determine_base_packages(
        &blobfs,
        &fuchsia_component::client::connect_to_protocol::<fboot::ArgumentsMarker>()
            .context("failed to connect to fuchsia.boot/Arguments")?,
    )
    .await
    .context("determine base packages")?;
    let authenticator = crate::context_authenticator::ContextAuthenticator::new();

    let scope = package_directory::ExecutionScope::new();
    let mut service_fs = fuchsia_component::server::ServiceFs::new_local();
    service_fs.add_remote(
        "shell-commands-bin",
        shell_commands_bin_dir(&base_packages, scope.clone(), blobfs.clone())
            .await
            .context("getting shell-commands-bin dir")?,
    );
    service_fs
        .dir("svc")
        .add_fidl_service(Services::PackageResolver)
        .add_fidl_service(Services::ComponentResolver);
    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;
    let () = service_fs
        .for_each_concurrent(None, |request| async {
            match request {
                Services::PackageResolver(stream) => {
                    package::serve_request_stream(
                        stream,
                        &base_packages,
                        authenticator.clone(),
                        &blobfs,
                    )
                    .unwrap_or_else(|e| error!("failed to serve package resolver request: {:#}", e))
                    .await
                }
                Services::ComponentResolver(stream) => {
                    component::serve_request_stream(
                        stream,
                        &base_packages,
                        authenticator.clone(),
                        &blobfs,
                    )
                    .unwrap_or_else(|e| {
                        error!("failed to serve component resolver request: {:#}", e)
                    })
                    .await
                }
            }
        })
        .await;
    let () = scope.wait().await;

    Ok(())
}

/// Panics if a base package has a non-zero variant.
/// The URLs in the returned map will not have a variant.
async fn determine_base_packages(
    blobfs: &blobfs::Client,
    boot_args: &fboot::ArgumentsProxy,
) -> anyhow::Result<HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>> {
    let system_image = system_image::SystemImage::new(blobfs.clone(), boot_args)
        .await
        .context("failed to load system_image package")?;
    let static_packages =
        system_image.static_packages().await.context("failed to determine static packages")?;
    let base_repo = fuchsia_url::RepositoryUrl::parse_host("fuchsia.com".into())
        .expect("valid repository hostname");
    Ok(HashMap::from_iter(
        static_packages
            .into_contents()
            .chain([(system_image::SystemImage::package_path(), *system_image.hash())])
            .map(|(path, hash)| {
                let (name, variant) = path.into_name_and_variant();
                // TODO(fxbug.dev/53911) Remove variant checks when variant concept is deleted.
                if !variant.is_zero() {
                    panic!("base package variants must be zero: {name} {variant}");
                }
                (fuchsia_url::UnpinnedAbsolutePackageUrl::new(base_repo.clone(), name, None), hash)
            }),
    ))
}

async fn shell_commands_bin_dir(
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    scope: package_directory::ExecutionScope,
    blobfs: blobfs::Client,
) -> anyhow::Result<fio::DirectoryProxy> {
    let (client, server) =
        fidl::endpoints::create_proxy::<fio::DirectoryMarker>().context("create proxy")?;
    let Some(hash) = base_packages.get(
        &"fuchsia-pkg://fuchsia.com/shell-commands"
            .parse()
            .expect("valid url")
        ) else {
        tracing::warn!(
            "no 'shell-commands' package in base, so exposed 'shell-commands-bin' directory will \
             close connections"
        );
        return Ok(client);
    };
    package_directory::serve_path(
        scope,
        blobfs,
        *hash,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        package_directory::VfsPath::validate_and_split("bin").expect("valid path"),
        server.into_channel().into(),
    )
    .await
    .context("serving shell-commands bin dir")?;
    Ok(client)
}

enum Services {
    PackageResolver(fpkg::PackageResolverRequestStream),
    ComponentResolver(fcomponent_resolution::ResolverRequestStream),
}

#[derive(thiserror::Error, Debug)]
enum ResolverError {
    #[error("invalid component URL")]
    InvalidUrl(#[from] fuchsia_url::errors::ParseError),

    #[error("component URL with package hash not supported")]
    PackageHashNotSupported,

    #[error("component not found")]
    ComponentNotFound(#[source] mem_util::FileError),

    #[error("couldn't parse component manifest")]
    ParsingManifest(#[source] fidl::Error),

    #[error("couldn't find config values")]
    ConfigValuesNotFound(#[source] mem_util::FileError),

    #[error("config source missing or invalid")]
    InvalidConfigSource,

    #[error("unsupported config source: {0:?}")]
    UnsupportedConfigSource(fcomponent_decl::ConfigValueSource),

    #[error("failed to read the manifest")]
    ReadManifest(#[source] mem_util::DataError),

    #[error("failed to create FIDL endpoints")]
    CreateEndpoints(#[source] fidl::Error),

    #[error("serve package directory")]
    ServePackageDirectory(#[source] package_directory::Error),

    #[error("create package directory")]
    CreatePackageDirectory(#[source] package_directory::Error),

    #[error("context must be empty when resolving absolute URL")]
    ContextWithAbsoluteUrl,

    #[error("subpackage name was not found in the package's subpackage list")]
    SubpackageNotFound,

    #[error("failed to read abi revision")]
    AbiRevision(#[source] fidl_fuchsia_component_abi_ext::AbiRevisionFileError),

    #[error("the package URL was not found in the base package index")]
    PackageNotInBase(fuchsia_url::AbsolutePackageUrl),

    #[error("failed to read the superpackage's subpackage manifest")]
    ReadingSubpackageManifest(#[from] package_directory::SubpackagesError),

    #[error("invalid context")]
    InvalidContext(#[from] crate::context_authenticator::ContextAuthenticatorError),

    #[error("resolve must be called with an absolute (not relative) url")]
    AbsoluteUrlRequired,

    #[error("failed to convert proxy to channel")]
    ConvertProxyToChannel,
}

impl From<&ResolverError> for fcomponent_resolution::ResolverError {
    fn from(err: &ResolverError) -> fcomponent_resolution::ResolverError {
        use {fcomponent_resolution::ResolverError as ferror, ResolverError::*};
        match err {
            InvalidUrl(_)
            | PackageHashNotSupported
            | InvalidContext(_)
            | AbsoluteUrlRequired
            | ContextWithAbsoluteUrl => ferror::InvalidArgs,
            ComponentNotFound(_) => ferror::ManifestNotFound,
            ConfigValuesNotFound(_) => ferror::ConfigValuesNotFound,
            ParsingManifest(_) | UnsupportedConfigSource(_) | InvalidConfigSource => {
                ferror::InvalidManifest
            }
            ReadManifest(_)
            | CreateEndpoints(_)
            | ServePackageDirectory(_)
            | CreatePackageDirectory(_)
            | ReadingSubpackageManifest(_) => ferror::Io,
            ConvertProxyToChannel => ferror::Internal,
            SubpackageNotFound | PackageNotInBase(_) => ferror::PackageNotFound,
            AbiRevision(_) => ferror::InvalidAbiRevision,
        }
    }
}

impl From<ResolverError> for fcomponent_resolution::ResolverError {
    fn from(err: ResolverError) -> fcomponent_resolution::ResolverError {
        (&err).into()
    }
}

impl From<&ResolverError> for fpkg::ResolveError {
    fn from(err: &ResolverError) -> fpkg::ResolveError {
        use {fpkg::ResolveError as ferror, ResolverError::*};
        match err {
            InvalidUrl(_) | PackageHashNotSupported | AbsoluteUrlRequired => ferror::InvalidUrl,
            ComponentNotFound(_)
            | ConfigValuesNotFound(_)
            | AbiRevision(_)
            | ParsingManifest(_)
            | UnsupportedConfigSource(_)
            | InvalidConfigSource
            | ConvertProxyToChannel => ferror::Internal,
            ReadManifest(_)
            | CreateEndpoints(_)
            | ServePackageDirectory(_)
            | CreatePackageDirectory(_)
            | ReadingSubpackageManifest(_) => ferror::Io,
            PackageNotInBase(_) | SubpackageNotFound => ferror::PackageNotFound,
            ContextWithAbsoluteUrl | InvalidContext(_) => ferror::InvalidContext,
        }
    }
}
