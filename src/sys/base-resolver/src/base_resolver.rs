// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, Context},
    fidl::endpoints::{ClientEnd, Proxy},
    fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_resolution::{
        self as fresolution, ResolverRequest, ResolverRequestStream,
    },
    fidl_fuchsia_io as fio,
    fidl_fuchsia_pkg::PackageCacheMarker,
    fidl_fuchsia_pkg_ext::BasePackageIndex,
    fuchsia_component::{client::connect_to_protocol, server::ServiceFs},
    fuchsia_pkg::{
        transitional::{context_bytes_from_subpackages_map, subpackages_map_from_context_bytes},
        PackageDirectory, PackageName,
    },
    fuchsia_url::{ComponentUrl, PackageUrl},
    futures::prelude::*,
    tracing::*,
    version_history::AbiRevision,
};

fn context_to_string(context: &fresolution::Context) -> String {
    String::from_utf8(context.bytes.clone()).unwrap_or_else(|_| hex::encode(&context.bytes))
}

#[derive(Debug, Copy, Clone)]
enum ResolutionRestrictions {
    OnlyBasePackages,
    None,
}

pub(crate) async fn main() -> anyhow::Result<()> {
    info!("started");

    let resolution_restrictions = {
        let config = base_resolver_config::Config::take_from_startup_handle();
        let inspector = fuchsia_inspect::component::inspector();
        inspector.root().record_child("config", |node| config.record_inspect(node));
        match config.only_base_packages {
            true => ResolutionRestrictions::OnlyBasePackages,
            false => ResolutionRestrictions::None,
        }
    };

    let mut service_fs = ServiceFs::new_local();
    service_fs.dir("svc").add_fidl_service(Services::BaseResolver);
    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;
    let () = service_fs
        .for_each_concurrent(None, |request| async {
            match request {
                Services::BaseResolver(stream) => {
                    serve(stream, resolution_restrictions)
                        .unwrap_or_else(|e| {
                            error!("failed to serve base resolver request: {:#}", e)
                        })
                        .await
                }
            }
        })
        .await;

    Ok(())
}

enum Services {
    BaseResolver(ResolverRequestStream),
}

async fn serve(
    mut stream: ResolverRequestStream,
    resolution_restrictions: ResolutionRestrictions,
) -> anyhow::Result<()> {
    let pkg_cache =
        connect_to_protocol::<PackageCacheMarker>().context("error connecting to package cache")?;
    let base_package_index = BasePackageIndex::from_proxy(&pkg_cache)
        .await
        .context("failed to load base package index")?;
    let packages_dir = fuchsia_fs::directory::open_in_namespace(
        "/pkgfs/packages",
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
    )
    .context("failed to open /pkgfs/packages")?;
    while let Some(request) =
        stream.try_next().await.context("failed to read request from FIDL stream")?
    {
        match request {
            ResolverRequest::Resolve { component_url, responder } => {
                let mut result = resolve_component(
                    &component_url,
                    &packages_dir,
                    &base_package_index,
                    resolution_restrictions,
                )
                .await
                .map_err(|err| (&err).into());
                responder.send(&mut result).context("failed sending response")?;
            }
            ResolverRequest::ResolveWithContext { component_url, context, responder } => {
                let mut result = resolve_component_with_context(
                    &component_url,
                    &context,
                    &packages_dir,
                    &base_package_index,
                    resolution_restrictions,
                )
                .await
                .map_err(|err| (&err).into());
                responder.send(&mut result).context("failed sending response")?;
            }
        };
    }
    Ok(())
}

async fn resolve_component(
    component_url: &str,
    packages_dir: &fio::DirectoryProxy,
    base_package_index: &BasePackageIndex,
    resolution_restrictions: ResolutionRestrictions,
) -> Result<fresolution::Component, crate::ResolverError> {
    let res = resolve_component_async(
        component_url,
        None,
        packages_dir,
        base_package_index,
        resolution_restrictions,
    )
    .await;
    if let Err(err) = &res {
        error!("failed to resolve component URL {}: {:?}", &component_url, err);
    }
    res
}

async fn resolve_component_with_context(
    component_url: &str,
    context: &fresolution::Context,
    packages_dir: &fio::DirectoryProxy,
    base_package_index: &BasePackageIndex,
    resolution_restrictions: ResolutionRestrictions,
) -> Result<fresolution::Component, crate::ResolverError> {
    let res = resolve_component_async(
        component_url,
        Some(context),
        packages_dir,
        base_package_index,
        resolution_restrictions,
    )
    .await;
    if let Err(err) = &res {
        error!(
            "failed to resolve component URL {} with context {}: {:?}",
            &component_url,
            context_to_string(&context),
            err
        );
    }
    res
}

async fn resolve_component_async(
    component_url_str: &str,
    some_incoming_context: Option<&fresolution::Context>,
    packages_dir: &fio::DirectoryProxy,
    base_package_index: &BasePackageIndex,
    resolution_restrictions: ResolutionRestrictions,
) -> Result<fresolution::Component, crate::ResolverError> {
    let component_url = ComponentUrl::parse(component_url_str)?;
    let package = resolve_package_async(
        component_url.package_url(),
        some_incoming_context,
        packages_dir,
        base_package_index,
        resolution_restrictions,
    )
    .await?;

    let data = mem_util::open_file_data(&package.dir, &component_url.resource())
        .await
        .map_err(crate::ResolverError::ComponentNotFound)?;
    let raw_bytes = mem_util::bytes_from_data(&data).map_err(crate::ResolverError::ReadManifest)?;
    let decl: fdecl::Component =
        fidl::encoding::unpersist(&raw_bytes[..]).map_err(crate::ResolverError::ParsingManifest)?;

    let config_values = if let Some(config_decl) = decl.config.as_ref() {
        // if we have a config declaration, we need to read the value file from the package dir
        let strategy =
            config_decl.value_source.as_ref().ok_or(crate::ResolverError::InvalidConfigSource)?;
        let config_path = match strategy {
            fdecl::ConfigValueSource::PackagePath(path) => path,
            other => return Err(crate::ResolverError::UnsupportedConfigSource(other.to_owned())),
        };
        Some(
            mem_util::open_file_data(&package.dir, &config_path)
                .await
                .map_err(crate::ResolverError::ConfigValuesNotFound)?,
        )
    } else {
        None
    };
    let abi_revision =
        fidl_fuchsia_component_abi_ext::read_abi_revision_optional(&package.dir, AbiRevision::PATH)
            .await
            .map_err(crate::ResolverError::AbiRevision)?;
    let package_dir = ClientEnd::new(
        package.dir.into_channel().expect("could not convert proxy to channel").into_zx_channel(),
    );
    Ok(fresolution::Component {
        url: Some(component_url_str.to_string()),
        resolution_context: Some(package.context),
        decl: Some(data),
        package: Some(fresolution::Package {
            url: Some(component_url.package_url().to_string()),
            directory: Some(package_dir),
            ..fresolution::Package::EMPTY
        }),
        config_values,
        abi_revision,
        ..fresolution::Component::EMPTY
    })
}

#[derive(Debug)]
struct ResolvedPackage {
    dir: fio::DirectoryProxy,
    context: fresolution::Context,
}

async fn resolve_package_async(
    package_url: &PackageUrl,
    some_incoming_context: Option<&fresolution::Context>,
    packages_dir: &fio::DirectoryProxy,
    base_package_index: &BasePackageIndex,
    resolution_restrictions: ResolutionRestrictions,
) -> Result<ResolvedPackage, crate::ResolverError> {
    let (package_name, some_variant) = match package_url {
        PackageUrl::Relative(relative) => {
            let context = some_incoming_context.ok_or_else(|| {
                crate::ResolverError::RelativeUrlMissingContext(package_url.to_string())
            })?;
            // TODO(fxbug.dev/101492): Update base-resolver to use blobfs directly,
            // and allow subpackage lookup to resolve subpackages from blobfs via
            // the blobid (package hash). Then base-resolver will no longer need
            // access to pkgfs-packages or to the package index (from PackageCache).
            let subpackage_hashes = subpackages_map_from_context_bytes(&context.bytes)
                .map_err(|err| crate::ResolverError::ReadingContext(err))?;
            let hash = subpackage_hashes.get(relative).ok_or_else(|| {
                crate::ResolverError::SubpackageNotFound(anyhow::format_err!(
                    "Subpackage '{}' not found in context: {:?}",
                    relative,
                    subpackage_hashes
                ))
            })?;
            let absolute = base_package_index.get_url(&(*hash).into()).ok_or_else(|| {
                crate::ResolverError::SubpackageNotInBase(anyhow::format_err!(
                    "Subpackage '{}' with hash '{}' is not in base",
                    relative,
                    hash
                ))
            })?;
            (absolute.name(), absolute.variant())
        }
        PackageUrl::Absolute(absolute) => {
            if absolute.host() != "fuchsia.com" {
                return Err(crate::ResolverError::UnsupportedRepo);
            }
            if absolute.hash().is_some() {
                return Err(crate::ResolverError::PackageHashNotSupported);
            }
            if absolute.name().as_ref().starts_with(PackageName::PREFIX_FOR_INDEXED_SUBPACKAGES) {
                return Err(crate::ResolverError::AbsoluteUrlWithReservedName);
            }
            match resolution_restrictions {
                ResolutionRestrictions::OnlyBasePackages => {
                    if base_package_index.is_unpinned_base_package(absolute).is_none() {
                        Err(crate::ResolverError::PackageNotInBase(absolute.clone()))?
                    }
                }
                ResolutionRestrictions::None => {}
            }
            (absolute.name(), absolute.variant())
        }
    };
    // Package contents are available at `packages/$PACKAGE_NAME/0`.
    let dir = fuchsia_fs::directory::open_directory(
        packages_dir,
        &format!(
            "{}/{}",
            package_name.as_ref(),
            some_variant.as_ref().map(|v| v.as_ref()).unwrap_or("0")
        ),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
    )
    .await
    .map_err(crate::ResolverError::PackageNotFound)?;
    let package_dir = PackageDirectory::from_proxy(dir);
    let context = fabricate_package_context(&package_dir)
        .map_err(|err| crate::ResolverError::CreatingContext(anyhow::anyhow!(err)))
        .await?;
    Ok(ResolvedPackage { dir: package_dir.into_proxy(), context })
}

async fn fabricate_package_context(
    package_dir: &PackageDirectory,
) -> anyhow::Result<fresolution::Context> {
    let meta = package_dir.meta_subpackages().await?;
    Ok(fresolution::Context {
        bytes: context_bytes_from_subpackages_map(&meta.into_subpackages())?.unwrap_or(vec![]),
    })
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fidl::encoding::persist,
        fidl::endpoints::{create_proxy, ServerEnd},
        fidl::prelude::*,
        fidl_fuchsia_component_config as fconfig, fidl_fuchsia_component_decl as fdecl,
        fidl_fuchsia_pkg_ext::BlobId,
        fuchsia_hash::Hash,
        fuchsia_pkg::MetaSubpackages,
        fuchsia_url::{RelativePackageUrl, UnpinnedAbsolutePackageUrl},
        fuchsia_zircon::Status,
        maplit::hashmap,
        std::{collections::HashMap, iter::FromIterator, str::FromStr, sync::Arc},
    };

    const SUBPACKAGE_NAME: &'static str = "my_subpackage";
    const SUBPACKAGE_HASH: &'static str =
        "facefacefacefacefacefacefacefacefacefacefacefacefacefacefaceface";
    const OTHER_SUBPACKAGE_HASH: &'static str =
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

    /// A DirectoryEntry implementation that checks whether an expected set of flags
    /// are set in the Open request.
    struct FlagVerifier(fio::OpenFlags);

    impl vfs::directory::entry::DirectoryEntry for FlagVerifier {
        fn open(
            self: Arc<Self>,
            _scope: vfs::execution_scope::ExecutionScope,
            flags: fio::OpenFlags,
            _path: vfs::path::Path,
            server_end: ServerEnd<fio::NodeMarker>,
        ) {
            let status = if flags & self.0 != self.0 { Status::INVALID_ARGS } else { Status::OK };
            let stream = server_end.into_stream().expect("failed to create stream");
            let control_handle = stream.control_handle();
            control_handle
                .send_on_open_(
                    status.into_raw(),
                    Some(&mut fio::NodeInfoDeprecated::Directory(fio::DirectoryObject {})),
                )
                .expect("failed to send OnOpen event");
            control_handle.shutdown_with_epitaph(status);
        }

        fn entry_info(&self) -> vfs::directory::entry::EntryInfo {
            vfs::directory::entry::EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        }
    }

    /// Serve a pseudo_dir with `RIGHT_EXECUTABLE` permissions, which can
    /// emulate `/pkgfs/packages` for tests.
    fn serve_executable_dir(
        pseudo_dir: Arc<dyn vfs::directory::entry::DirectoryEntry>,
    ) -> fio::DirectoryProxy {
        let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>()
            .expect("failed to create DirectoryProxy/Server pair");
        let () = pseudo_dir.open(
            vfs::execution_scope::ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
            vfs::path::Path::dot(),
            ServerEnd::new(server_end.into_channel()),
        );
        proxy
    }

    fn empty_index() -> BasePackageIndex {
        BasePackageIndex::create_mock(HashMap::new())
    }

    #[fuchsia::test]
    async fn fails_to_resolve_package_unsupported_repo() {
        let packages_dir = serve_executable_dir(vfs::pseudo_directory! {});
        assert_matches!(
            resolve_component(
                "fuchsia-pkg://fuchsia.ca/test-package#meta/foo.cm",
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::UnsupportedRepo)
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_invalid_url() {
        let packages_dir = serve_executable_dir(vfs::pseudo_directory! {});
        assert_matches!(
            resolve_component(
                "fuchsia://fuchsia.com/foo#meta/bar.cm",
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::InvalidUrl(_))
        );
        assert_matches!(
            resolve_component(
                "fuchsia-pkg://fuchsia.com/foo",
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::InvalidUrl(_))
        );
        assert_matches!(
            resolve_component(
                "fuchsia-pkg://fuchsia.com/#meta/bar.cm",
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::InvalidUrl(_))
        );
        assert_matches!(
            resolve_component(
                "fuchsia-pkg://fuchsia.ca/foo#meta/bar.cm",
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::UnsupportedRepo)
        );
        assert_matches!(
            resolve_component(
                &format!(
                    "fuchsia-pkg://fuchsia.com/{}{}#meta/bar.cm",
                    PackageName::PREFIX_FOR_INDEXED_SUBPACKAGES,
                    SUBPACKAGE_HASH
                ),
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::AbsoluteUrlWithReservedName)
        );

        let url_with_hash = concat!(
            "fuchsia-pkg://fuchsia.com/test-package",
            "?hash=f241b31d5913b66c90a44d44537d6bec62672e1f05dbc4c4f22b863b01c68749",
            "#meta/test.cm"
        );
        assert_matches!(
            resolve_component(
                url_with_hash,
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::PackageHashNotSupported)
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_package_not_found() {
        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        assert_matches!(
            resolve_component(
                "fuchsia-pkg://fuchsia.com/missing-package#meta/foo.cm",
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::PackageNotFound(_))
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_missing_manifest() {
        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        assert_matches!(
            resolve_component(
                "fuchsia-pkg://fuchsia.com/test-package#meta/bar.cm",
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::ComponentNotFound(_))
        );
    }

    #[fuchsia::test]
    async fn resolves_component_large_manifest() {
        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        assert_matches!(
            resolve_component(
                "fuchsia-pkg://fuchsia.com/test-package#meta/large.cm",
                &packages_dir, &empty_index(), ResolutionRestrictions::None
            )
            .await,
            Ok(fresolution::Component { decl: Some(decl_as_bytes), .. }) => {
                assert_eq!(very_long_name(), fidl::encoding::unpersist::<fdecl::Component>(
                    &mem_util::bytes_from_data(&decl_as_bytes).unwrap()[..]
                )
                .unwrap()
                .program
                .unwrap()
                .runner
                .unwrap())
            }
        );
    }

    #[fuchsia::test]
    async fn resolves_component_file_manifest() {
        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        assert_matches!(
            resolve_component(
                "fuchsia-pkg://fuchsia.com/test-package#meta/foo.cm",
                &packages_dir,
                &empty_index(),
                ResolutionRestrictions::None
            )
            .await,
            Ok(fresolution::Component {
                decl: Some(fidl_fuchsia_mem::Data::Buffer(fidl_fuchsia_mem::Buffer { .. })),
                ..
            })
        );
    }

    #[fuchsia::test]
    async fn resolves_component_in_subpackage() {
        let parent_component_url = "fuchsia-pkg://fuchsia.com/test-package#meta/foo.cm";
        let subpackaged_component_url = SUBPACKAGE_NAME.to_string() + "#meta/subfoo.cm";

        // Set up the base package index with the subpackage's hash that will
        // be requested. The subpackage's top-level package name does not need
        // to match `SUBPACKAGE_NAME`.
        let subpackage_as_base_package_url: UnpinnedAbsolutePackageUrl =
            "fuchsia-pkg://fuchsia.com/toplevel-subpackage".parse().unwrap();
        let subpackage_blob_id = BlobId::parse(SUBPACKAGE_HASH).unwrap();
        let index = hashmap! {
            subpackage_as_base_package_url => subpackage_blob_id,
        };
        let base_package_index = BasePackageIndex::create_mock(index);

        let packages_dir = serve_executable_dir(build_fake_packages_dir_with_named_subpackage());
        let parent_component = resolve_component(
            parent_component_url,
            &packages_dir,
            &base_package_index,
            ResolutionRestrictions::None,
        )
        .await
        .expect("failed to resolve parent_component");

        assert_matches!(parent_component.resolution_context, Some(..));
        assert_matches!(
            resolve_component_with_context(
                &subpackaged_component_url,
                parent_component.resolution_context.as_ref().unwrap(),
                &packages_dir,
                &base_package_index,
                ResolutionRestrictions::None,
            )
            .await,
            Ok(fresolution::Component { decl: Some(..), .. }),
            "Could not resolve subpackaged component '{}' from context '{:?}'",
            subpackaged_component_url,
            parent_component.resolution_context.as_ref().map(|v| context_to_string(v))
        );
    }

    #[fuchsia::test]
    async fn resolves_component_in_anonymous_subpackage() {
        let parent_component_url = "fuchsia-pkg://fuchsia.com/test-package#meta/foo.cm";
        let subpackaged_component_url = SUBPACKAGE_NAME.to_string() + "#meta/subfoo.cm";

        // Set up the base package index with the subpackage's hash that will
        // be requested.
        //
        // `subpackage_as_base_package_url` is not resolveable, but is used
        // in this test as a BasePackageIndex entry that can be resolved as
        // a subpackage from ResolveWithContext.
        let subpackage_as_base_package_url_string = format!(
            "fuchsia-pkg://fuchsia.com/{}{}/0",
            PackageName::PREFIX_FOR_INDEXED_SUBPACKAGES,
            SUBPACKAGE_HASH
        );
        let subpackage_as_base_package_url: UnpinnedAbsolutePackageUrl =
            subpackage_as_base_package_url_string.parse().unwrap();
        let subpackage_blob_id = BlobId::parse(SUBPACKAGE_HASH).unwrap();
        let index = hashmap! {
            subpackage_as_base_package_url => subpackage_blob_id,
        };
        let base_package_index = BasePackageIndex::create_mock(index);

        let packages_dir =
            serve_executable_dir(build_fake_packages_dir_with_anonymous_subpackage());

        assert_matches!(
            resolve_component(
                &format!("{subpackage_as_base_package_url_string}#meta/bar.cm"),
                &packages_dir,
                &base_package_index,
                ResolutionRestrictions::None
            )
            .await,
            Err(crate::ResolverError::AbsoluteUrlWithReservedName)
        );

        let parent_component = resolve_component(
            parent_component_url,
            &packages_dir,
            &base_package_index,
            ResolutionRestrictions::None,
        )
        .await
        .expect("failed to resolve parent_component");

        assert_matches!(parent_component.resolution_context, Some(..));
        assert_matches!(
            resolve_component_with_context(
                &subpackaged_component_url,
                parent_component.resolution_context.as_ref().unwrap(),
                &packages_dir,
                &base_package_index,
                ResolutionRestrictions::None,
            )
            .await,
            Ok(fresolution::Component { decl: Some(..), .. }),
            "Could not resolve subpackaged component '{}' from context '{:?}'",
            subpackaged_component_url,
            parent_component.resolution_context.as_ref().map(|v| context_to_string(v))
        );
    }

    #[fuchsia::test]
    async fn resolves_component_in_subpackage_with_unique_abi_revision() {
        let parent_component_url = "fuchsia-pkg://fuchsia.com/test-package#meta/foo.cm";
        let subpackaged_component_url = SUBPACKAGE_NAME.to_string() + "#meta/subfoo.cm";

        let subpackage_as_base_package_url: UnpinnedAbsolutePackageUrl =
            "fuchsia-pkg://fuchsia.com/toplevel-subpackage".parse().unwrap();
        let subpackage_blob_id = BlobId::parse(SUBPACKAGE_HASH).unwrap();
        let index = hashmap! {
            subpackage_as_base_package_url => subpackage_blob_id,
        };
        let base_package_index = BasePackageIndex::create_mock(index);

        // Test ABI revision value of the top-level package component
        let packages_dir = serve_executable_dir(build_fake_packages_dir_with_named_subpackage());
        let parent_component = resolve_component(
            parent_component_url,
            &packages_dir,
            &base_package_index,
            ResolutionRestrictions::None,
        )
        .await
        .unwrap();
        assert_matches!(parent_component, fresolution::Component { abi_revision: Some(0), .. });

        // Test ABI revision value of the sub-packaged component
        let subpackaged_component = resolve_component_with_context(
            &subpackaged_component_url,
            &parent_component.resolution_context.unwrap(),
            &packages_dir,
            &base_package_index,
            ResolutionRestrictions::None,
        )
        .await
        .unwrap();
        assert_matches!(
            subpackaged_component,
            fresolution::Component { abi_revision: Some(1), .. }
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_in_subpackage_not_in_base() {
        let parent_component_url = "fuchsia-pkg://fuchsia.com/test-package#meta/foo.cm";
        let subpackaged_component_url = SUBPACKAGE_NAME.to_string() + "#meta/other_subfoo.cm";

        // Set up the base package index WITHOUT the subpackage's hash that will
        // be requested
        let other_subpackage_as_base_package_url: UnpinnedAbsolutePackageUrl =
            "fuchsia-pkg://fuchsia.com/some-other-package".parse().unwrap();
        let other_subpackage_blob_id = BlobId::parse(OTHER_SUBPACKAGE_HASH).unwrap();
        let index = hashmap! {
            other_subpackage_as_base_package_url => other_subpackage_blob_id,
        };
        let base_package_index = BasePackageIndex::create_mock(index);
        assert!(base_package_index.contains_package(&other_subpackage_blob_id));

        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        let parent_component = resolve_component(
            parent_component_url,
            &packages_dir,
            &base_package_index,
            ResolutionRestrictions::None,
        )
        .await
        .expect("failed to resolve parent_component");

        assert_matches!(parent_component.resolution_context, Some(..));
        assert_matches!(
            resolve_component_with_context(
                &subpackaged_component_url,
                parent_component.resolution_context.as_ref().unwrap(),
                &packages_dir,
                &base_package_index,
                ResolutionRestrictions::None,
            )
            .await,
            Err(crate::ResolverError::SubpackageNotInBase(..))
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_subpackage_name_not_in_parent_subpackages() {
        let parent_component_url = "fuchsia-pkg://fuchsia.com/test-package#meta/foo.cm";
        let subpackaged_component_url = "subpackage_not_in_parent#meta/other_subfoo.cm";

        // Set up the base package index with the subpackage's hash that will
        // be requested, but can't be resolved because it's not a subpackage
        // of the parent package. (The subpackage's top-level package name does
        // not need to match any package-specific subpackage name.)
        let other_subpackage_as_base_package_url: UnpinnedAbsolutePackageUrl =
            "fuchsia-pkg://fuchsia.com/toplevel-other-subpackage".parse().unwrap();
        let other_subpackage_blob_id = BlobId::parse(OTHER_SUBPACKAGE_HASH).unwrap();
        let index = hashmap! {
            other_subpackage_as_base_package_url => other_subpackage_blob_id,
        };
        let base_package_index = BasePackageIndex::create_mock(index);
        assert!(base_package_index.contains_package(&other_subpackage_blob_id));

        let packages_dir = serve_executable_dir(build_fake_packages_dir_with_named_subpackage());
        let parent_component = resolve_component(
            parent_component_url,
            &packages_dir,
            &base_package_index,
            ResolutionRestrictions::None,
        )
        .await
        .expect("failed to resolve parent_component");

        assert_matches!(parent_component.resolution_context, Some(..));
        assert_matches!(
            resolve_component_with_context(
                &subpackaged_component_url,
                parent_component.resolution_context.as_ref().unwrap(),
                &packages_dir,
                &base_package_index,
                ResolutionRestrictions::None,
            )
            .await,
            Err(crate::ResolverError::SubpackageNotFound(..))
        );
    }

    #[fuchsia::test]
    async fn resolves_component_with_config() {
        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        let component = resolve_component(
            "fuchsia-pkg://fuchsia.com/test-package#meta/foo-with-config.cm",
            &packages_dir,
            &empty_index(),
            ResolutionRestrictions::None,
        )
        .await
        .unwrap();
        assert_matches!(
            component,
            fresolution::Component { decl: Some(..), config_values: Some(..), .. }
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_missing_config_values() {
        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        let error = resolve_component(
            "fuchsia-pkg://fuchsia.com/test-package#meta/foo-without-config.cm",
            &packages_dir,
            &empty_index(),
            ResolutionRestrictions::None,
        )
        .await
        .unwrap_err();
        assert_matches!(error, crate::ResolverError::ConfigValuesNotFound(..));
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_bad_config_source() {
        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        let error = resolve_component(
            "fuchsia-pkg://fuchsia.com/test-package#meta/foo-with-bad-config.cm",
            &packages_dir,
            &empty_index(),
            ResolutionRestrictions::None,
        )
        .await
        .unwrap_err();
        assert_matches!(error, crate::ResolverError::InvalidConfigSource);
    }

    #[fuchsia::test]
    async fn resolves_base_component_with_enforcement_enabled() {
        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        let component = resolve_component(
            "fuchsia-pkg://fuchsia.com/test-package#meta/foo-with-config.cm",
            &packages_dir,
            &BasePackageIndex::create_mock(HashMap::from_iter([(
                "fuchsia-pkg://fuchsia.com/test-package".parse().unwrap(),
                BlobId::from([0u8; 32]),
            )])),
            ResolutionRestrictions::OnlyBasePackages,
        )
        .await
        .unwrap();
        assert_matches!(
            component,
            fresolution::Component { decl: Some(..), config_values: Some(..), .. }
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_non_base_component() {
        let packages_dir = serve_executable_dir(build_fake_packages_dir());
        let error = resolve_component(
            "fuchsia-pkg://fuchsia.com/test-package#meta/foo-with-bad-config.cm",
            &packages_dir,
            &BasePackageIndex::create_mock(HashMap::from_iter([(
                "fuchsia-pkg://fuchsia.com/wrong-package".parse().unwrap(),
                BlobId::from([0u8; 32]),
            )])),
            ResolutionRestrictions::OnlyBasePackages,
        )
        .await
        .unwrap_err();
        assert_matches!(error, crate::ResolverError::PackageNotInBase(_));
    }

    fn build_fake_packages_dir() -> Arc<dyn vfs::directory::entry::DirectoryEntry> {
        let parent_package = build_fake_package_dir();
        vfs::pseudo_directory! {
            "test-package" => vfs::pseudo_directory! {
                "0" => parent_package,
            },
        }
    }

    fn build_fake_packages_dir_with_named_subpackage(
    ) -> Arc<dyn vfs::directory::entry::DirectoryEntry> {
        let subpackage = build_fake_subpackage_dir();
        let parent_package = build_fake_package_dir();
        vfs::pseudo_directory! {
            "test-package" => vfs::pseudo_directory! {
                "0" => parent_package,
            },
            "toplevel-subpackage" => vfs::pseudo_directory! {
                "0" => subpackage,
            },
        }
    }

    fn build_fake_packages_dir_with_anonymous_subpackage(
    ) -> Arc<dyn vfs::directory::entry::DirectoryEntry> {
        let subpackage = build_fake_subpackage_dir();
        let parent_package = build_fake_package_dir();
        vfs::pseudo_directory! {
            "test-package" => vfs::pseudo_directory! {
                "0" => parent_package,
            },
            &format!(
                "{}{}", PackageName::PREFIX_FOR_INDEXED_SUBPACKAGES, SUBPACKAGE_HASH
            ) => vfs::pseudo_directory! {
                "0" => subpackage,
            },
        }
    }

    fn build_fake_package_dir() -> Arc<dyn vfs::directory::entry::DirectoryEntry> {
        let cm_bytes = persist(&mut fdecl::Component::EMPTY.clone())
            .expect("failed to encode ComponentDecl FIDL");

        let subpackages = MetaSubpackages::from_iter(vec![(
            RelativePackageUrl::parse(SUBPACKAGE_NAME).unwrap(),
            Hash::from_str(SUBPACKAGE_HASH).unwrap(),
        )]);

        vfs::pseudo_directory! {
            "meta" => vfs::pseudo_directory! {
                "fuchsia.pkg" => vfs::pseudo_directory! {
                    "subpackages" => vfs::file::vmo::read_only_const(
                        &serde_json::to_vec(&subpackages).unwrap()
                    ),
                },
                "fuchsia.abi" => vfs::pseudo_directory! {
                  "abi-revision" => vfs::file::vmo::read_only_static(0u64.to_le_bytes()),
                },
                "foo.cm" => vfs::file::vmo::read_only_const(&cm_bytes),
                "foo-with-config.cm" => vfs::file::vmo::read_only_const(
                    &persist(
                        &mut fdecl::Component {
                            config: Some(fdecl::ConfigSchema {
                                value_source: Some(
                                    fdecl::ConfigValueSource::PackagePath(
                                        "meta/foo-with-config.cvf".to_string(),
                                    ),
                                ),
                                ..fdecl::ConfigSchema::EMPTY
                            }),
                            ..fdecl::Component::EMPTY
                        }
                    ).unwrap()
                ),
                "foo-with-config.cvf" => vfs::file::vmo::read_only_const(
                    &persist(
                        &mut fconfig::ValuesData {
                            ..fconfig::ValuesData::EMPTY
                        }
                    ).unwrap()
                ),
                "foo-with-bad-config.cm" => vfs::file::vmo::read_only_const(
                    &persist(
                        &mut fdecl::Component {
                            config: Some(fdecl::ConfigSchema {
                                ..fdecl::ConfigSchema::EMPTY
                            }),
                            ..fdecl::Component::EMPTY
                        }
                    ).unwrap()
                ),
                "foo-without-config.cm" => vfs::file::vmo::read_only_const(
                    &persist(
                        &mut fdecl::Component {
                            config: Some(fdecl::ConfigSchema {
                                value_source: Some(
                                    fdecl::ConfigValueSource::PackagePath(
                                        "doesnt-exist.cvf".to_string(),
                                    ),
                                ),
                                ..fdecl::ConfigSchema::EMPTY
                            }),
                            ..fdecl::Component::EMPTY
                        }
                    ).unwrap()
                ),
                "large.cm" => vfs::file::vmo::read_only_const(
                    &persist(
                        &mut fdecl::Component {
                            program: Some(fdecl::Program {
                                runner: Some(very_long_name()),
                                ..fdecl::Program::EMPTY
                            }),
                            ..fdecl::Component::EMPTY
                        }
                    ).unwrap()
                ),
            }
        }
    }

    // Any manifest that includes this name (70k bytes long) will necessarily be larger than the
    // 64KiB channel message limit, forcing that manifest to be transported via VMO in all cases.
    fn very_long_name() -> String {
        "very_long_name_".repeat(5_000)
    }

    fn build_fake_subpackage_dir() -> Arc<dyn vfs::directory::entry::DirectoryEntry> {
        let cm_bytes = persist(&mut fdecl::Component::EMPTY.clone())
            .expect("failed to encode ComponentDecl FIDL");

        vfs::pseudo_directory! {
            "meta" => vfs::pseudo_directory! {
                "subfoo.cm" => vfs::file::vmo::read_only_const(&cm_bytes),
                "fuchsia.abi" => vfs::pseudo_directory! {
                  "abi-revision" => vfs::file::vmo::read_only_static(1u64.to_le_bytes()),
                },
            }
        }
    }
}
