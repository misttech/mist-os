// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::{BuiltinCapability, CapabilityProvider, InternalCapabilityProvider};
use crate::model::component::WeakComponentInstance;
use crate::model::resolver::Resolver;
use anyhow::{format_err, Error};
use async_trait::async_trait;
use fidl::endpoints::{ClientEnd, Proxy, ServerEnd};
use fuchsia_pkg::PackagePath;
use fuchsia_url::boot_url::BootUrl;
use fuchsia_url::{PackageName, PackageVariant};
use futures::TryStreamExt;
use routing::capability_source::InternalCapability;
use routing::resolving::{self, ComponentAddress, ResolvedComponent, ResolverError};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use system_image::{Bootfs, PathHashMapping};
use version_history::AbiRevision;
use {
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_resolution as fresolution,
    fidl_fuchsia_io as fio, zx,
};

pub const SCHEME: &str = "fuchsia-boot";

/// The path for the bootfs package index relative to root of
/// the /boot directory.
const BOOT_PACKAGE_INDEX: &str = "data/bootfs_packages";

/// The subdirectory of /boot that holds all merkle-root named
/// blobs used by package resolution.
static BOOTFS_BLOB_DIR: &str = "blob";

/// Resolves component URLs with the "fuchsia-boot" scheme, which supports loading components from
/// the /boot directory in component_manager's namespace.
///
/// On a typical system, this /boot directory is the bootfs served from the contents of the
/// 'ZBI_TYPE_STORAGE_BOOTFS' ZBI item by bootsvc, the process which starts component_manager.
///
/// For unit and integration tests, the /pkg directory in component_manager's namespace may be used
/// to load components.
///
/// URL syntax:
/// - fuchsia-boot:///path/within/bootfs#meta/component.cm
#[derive(Clone, Debug)]
pub struct FuchsiaBootResolver {
    boot_proxy: fio::DirectoryProxy,
    boot_package_resolver: Option<Arc<BootPackageResolver>>,
}

impl FuchsiaBootResolver {
    /// Create a new FuchsiaBootResolver. This first checks whether the path passed in is present in
    /// the namespace, and returns Ok(None) if not present. For unit and integration tests, this
    /// path may point to /pkg.
    pub async fn new(path: &'static str) -> Result<Option<Self>, Error> {
        let bootfs_dir = Path::new(path);

        // TODO(97517): Remove this check if there is never a case for starting component manager
        // without a /boot dir in namespace.
        if !bootfs_dir.exists() {
            return Ok(None);
        }

        let boot_proxy = fuchsia_fs::directory::open_in_namespace(
            bootfs_dir.to_str().unwrap(),
            fio::PERM_READABLE | fio::PERM_EXECUTABLE,
        )?;

        Ok(Some(Self::new_from_directory(boot_proxy).await?))
    }

    /// Create a new FuchsiaBootResolver that resolves URLs within the given directory. Used for
    /// injection in unit tests.
    async fn new_from_directory(proxy: fio::DirectoryProxy) -> Result<Self, Error> {
        let boot_package_resolver = BootPackageResolver::try_instantiate(&proxy).await?;

        Ok(Self { boot_proxy: proxy, boot_package_resolver })
    }

    async fn resolve_unpackaged_component(
        &self,
        boot_url: BootUrl,
    ) -> Result<fresolution::Component, fresolution::ResolverError> {
        // When a component is unpacked, the root of its namespace is the root
        // of the /boot directory.
        let namespace_root = ".";

        // Set up the fuchsia-boot path as the component's "package" namespace.
        let path_proxy = fuchsia_fs::directory::open_directory_async(
            &self.boot_proxy,
            namespace_root,
            fio::PERM_READABLE | fio::PERM_EXECUTABLE,
        )
        .map_err(|_| fresolution::ResolverError::Internal)?;

        // unpackaged components resolved from the zbi are assigned the platform
        // abi revision
        let abi_revision = version_history_data::HISTORY.get_abi_revision_for_platform_components();

        self.construct_component(path_proxy, boot_url, Some(abi_revision)).await
    }

    async fn resolve_packaged_component(
        &self,
        boot_url: BootUrl,
    ) -> Result<fresolution::Component, fresolution::ResolverError> {
        // Package path is 'canonicalized' to ensure that it is relative, since absolute paths will
        // be (inconsistently) rejected by fuchsia.io methods.
        let canonicalized_package_path = fuchsia_fs::canonicalize_path(boot_url.path());
        match &self.boot_package_resolver {
            Some(boot_package_resolver) => {
                let package_dir_proxy =
                    boot_package_resolver.setup_package_dir(canonicalized_package_path).await?;
                // TODO(97517): when all bootfs components are packaged, abi_revision setting can be moved
                // into `construct_component()`.
                let abi_revision = fidl_fuchsia_component_abi_ext::read_abi_revision_optional(
                    &package_dir_proxy,
                    AbiRevision::PATH,
                )
                .await?;
                self.construct_component(package_dir_proxy, boot_url, abi_revision).await
            }
            _ => {
                tracing::warn!(
                    "Encountered a packaged bootfs component, but bootfs has no package index: {:?}",
                    canonicalized_package_path);
                return Err(fresolution::ResolverError::PackageNotFound);
            }
        }
    }

    async fn construct_component(
        &self,
        proxy: fio::DirectoryProxy,
        boot_url: BootUrl,
        abi_revision: Option<AbiRevision>,
    ) -> Result<fresolution::Component, fresolution::ResolverError> {
        let manifest = boot_url.resource().ok_or(fresolution::ResolverError::InvalidArgs)?;

        // Read the component manifest (.cm file) from the package-root.
        let data = mem_util::open_file_data(&proxy, &manifest)
            .await
            .map_err(|_| fresolution::ResolverError::ManifestNotFound)?;

        let decl_bytes =
            mem_util::bytes_from_data(&data).map_err(|_| fresolution::ResolverError::Io)?;

        let decl: fdecl::Component = fidl::unpersist(&decl_bytes[..])
            .map_err(|_| fresolution::ResolverError::InvalidManifest)?;

        let config_values = if let Some(config_decl) = decl.config.as_ref() {
            let strategy = config_decl
                .value_source
                .as_ref()
                .ok_or(fresolution::ResolverError::InvalidManifest)?;
            match strategy {
                // If we have to read the source from a package, do so.
                fdecl::ConfigValueSource::PackagePath(path) => Some(
                    mem_util::open_file_data(&proxy, path)
                        .await
                        .map_err(|_| fresolution::ResolverError::ConfigValuesNotFound)?,
                ),
                // We don't have to do anything for capability routing.
                fdecl::ConfigValueSource::Capabilities(_) => None,
                fdecl::ConfigValueSourceUnknown!() => {
                    return Err(fresolution::ResolverError::InvalidManifest);
                }
            }
        } else {
            None
        };
        Ok(fresolution::Component {
            url: Some(boot_url.to_string().into()),
            resolution_context: None,
            decl: Some(data),
            package: Some(fresolution::Package {
                // This call just strips the boot_url of the resource.
                url: Some(boot_url.root_url().to_string()),
                directory: Some(ClientEnd::new(proxy.into_channel().unwrap().into_zx_channel())),
                ..Default::default()
            }),
            config_values,
            abi_revision: abi_revision.map(Into::into),
            ..Default::default()
        })
    }

    async fn resolve_async(
        &self,
        component_url: &str,
    ) -> Result<fresolution::Component, fresolution::ResolverError> {
        // Parse URL.
        let url =
            BootUrl::parse(component_url).map_err(|_| fresolution::ResolverError::InvalidArgs)?;
        // Package path is 'canonicalized' to ensure that it is relative, since absolute paths will
        // be (inconsistently) rejected by fuchsia.io methods.
        let canonicalized_path = fuchsia_fs::canonicalize_path(url.path());

        match canonicalized_path {
            "." => {
                return self.resolve_unpackaged_component(url).await;
            }
            _ => {
                return self.resolve_packaged_component(url).await;
            }
        }
    }

    pub async fn serve(self, mut stream: fresolution::ResolverRequestStream) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                fresolution::ResolverRequest::Resolve { component_url, responder } => {
                    responder.send(self.resolve_async(&component_url).await)?;
                }
                fresolution::ResolverRequest::ResolveWithContext {
                    component_url,
                    context: _,
                    responder,
                } => {
                    // FuchsiaBootResolver ResolveWithContext currently ignores
                    // context, but should still resolve absolute URLs.
                    responder.send(self.resolve_async(&component_url).await)?;
                }
                fresolution::ResolverRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!(%ordinal, "Unknown Resolver request");
                }
            }
        }
        Ok(())
    }
}

impl BuiltinCapability for FuchsiaBootResolver {
    fn matches(&self, capability: &InternalCapability) -> bool {
        matches!(capability, InternalCapability::Resolver(n) if n.as_str() == "boot_resolver")
    }

    fn new_provider(&self, _target: WeakComponentInstance) -> Box<dyn CapabilityProvider> {
        Box::new(ComponentResolverCapabilityProvider::new(self.clone()))
    }
}

struct ComponentResolverCapabilityProvider {
    component_resolver: FuchsiaBootResolver,
}

impl ComponentResolverCapabilityProvider {
    pub fn new(component_resolver: FuchsiaBootResolver) -> Self {
        Self { component_resolver }
    }
}

#[async_trait]
impl InternalCapabilityProvider for ComponentResolverCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fresolution::ResolverMarker>::new(server_end);
        if let Err(error) = self.component_resolver.serve(server_end.into_stream().unwrap()).await {
            tracing::warn!(%error, "FuchsiaBootResolver::serve failed");
        }
    }
}

#[derive(Debug)]
struct BootPackageResolver {
    // Blobfs client exposing the bootfs
    // /boot/blob directory to package-directory interface.
    // TODO(97517): Refactor to an impl of NonMetaStorage.
    boot_blob_storage: fio::DirectoryProxy,
    // PathHashMapping encoding the index for boot package resolution.
    boot_package_index: PathHashMapping<Bootfs>,
}

impl BootPackageResolver {
    // Attempts to instantiate a BootPackageResolver.
    //
    // - The absence of a /boot/blob dir implies that there are no packages in the BootFS,
    // and boot resolver setup should still succeed.
    //
    // - The presence of a /boot/blob dir, but absence of a package index implies incorrect
    //   bootfs assembly, and produces a FuchsiaBootResolver instantiation error.
    async fn try_instantiate(proxy: &fio::DirectoryProxy) -> Result<Option<Arc<Self>>, Error> {
        // Check for the existence of a /boot/blob directory. Until we've started our migration,
        // it's a valid state for no packages to exist in the bootfs, in which case no blobs will
        // exist.
        if !fuchsia_fs::directory::dir_contains(proxy, BOOTFS_BLOB_DIR).await? {
            return Ok(None);
        }

        let boot_blob_storage = fuchsia_fs::directory::open_directory_async(
            &proxy,
            BOOTFS_BLOB_DIR,
            fio::PERM_READABLE | fio::PERM_EXECUTABLE,
        )
        .map_err(|err| format_err!("Bootfs blob directory existed, but converting it into a blob client for package resolution failed: {:?}", err))?;

        let boot_package_index =
            BootPackageResolver::extract_bootfs_index(&proxy).await.map_err(|err| {
                format_err!(
                    "Failed to extract a package index from a bootfs that contains packages: {:?}",
                    err
                )
            })?;

        Ok(Some(Arc::new(BootPackageResolver { boot_blob_storage, boot_package_index })))
    }

    /// Load `data/bootfs_packages` from /boot, if present.
    async fn extract_bootfs_index(
        boot_proxy: &fio::DirectoryProxy,
    ) -> Result<PathHashMapping<Bootfs>, Error> {
        let bootfs_package_index = fuchsia_fs::directory::open_file_async(
            &boot_proxy,
            BOOT_PACKAGE_INDEX,
            fio::PERM_READABLE,
        )?;

        let bootfs_package_contents = fuchsia_fs::file::read(&bootfs_package_index).await?;

        PathHashMapping::<Bootfs>::deserialize(&(*bootfs_package_contents))
            .map_err(|e| format_err!("Parsing bootfs index failed: {:?}", e))
    }

    async fn setup_package_dir(
        &self,
        canonicalized_package_path: &str,
    ) -> Result<fio::DirectoryProxy, fresolution::ResolverError> {
        let package_path = match PackageName::from_str(canonicalized_package_path) {
            Ok(package_name) => {
                PackagePath::from_name_and_variant(package_name, PackageVariant::zero())
            }
            Err(e) => {
                tracing::warn!("Bootfs package paths should be a single named segment: {:?}", e);
                return Err(fresolution::ResolverError::InvalidArgs);
            }
        };

        let meta_hash = self
            .boot_package_index
            .hash_for_package(&package_path)
            .ok_or(fresolution::ResolverError::PackageNotFound)?;

        let (proxy, server) = fidl::endpoints::create_proxy()
            .map_err(|_| fresolution::ResolverError::InvalidManifest)?;

        let blob_proxy = fuchsia_fs::directory::clone_no_describe(&self.boot_blob_storage, None)
            .map_err(|e| {
                tracing::warn!(
                    "Creating duplicate connection to /boot/blob directory failed: {:?}",
                    e
                );
                fresolution::ResolverError::Internal
            })?;

        let () = package_directory::serve(
            // scope is used to spawn an async task, which will continue until all features complete.
            package_directory::ExecutionScope::new(),
            blob_proxy,
            meta_hash,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
            server,
        )
        .await
        .map_err(|_| fresolution::ResolverError::Internal)?;

        Ok(proxy)
    }
}

#[async_trait]
impl Resolver for FuchsiaBootResolver {
    async fn resolve(
        &self,
        component_address: &ComponentAddress,
    ) -> Result<ResolvedComponent, ResolverError> {
        if component_address.is_relative_path() {
            return Err(ResolverError::UnexpectedRelativePath(component_address.url().to_string()));
        }
        let fresolution::Component { url, decl, package, config_values, abi_revision, .. } =
            self.resolve_async(component_address.url()).await?;
        let resolved_url = url.unwrap();
        let decl = decl.ok_or_else(|| {
            ResolverError::ManifestInvalid(
                anyhow::format_err!("missing manifest from resolved component").into(),
            )
        })?;
        let decl = resolving::read_and_validate_manifest(&decl)?;
        let config_values = if let Some(cv) = config_values {
            Some(resolving::read_and_validate_config_values(&cv)?)
        } else {
            None
        };
        Ok(ResolvedComponent {
            resolved_url,
            context_to_resolve_children: None,
            decl,
            package: package.map(|p| p.try_into()).transpose()?,
            config_values,
            abi_revision: abi_revision.map(Into::into),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::component::ComponentInstance;
    use crate::model::context::ModelContext;
    use crate::model::environment::Environment;
    use ::routing::resolving::ResolvedPackage;
    use assert_matches::assert_matches;
    use cm_rust::{FidlIntoNative, NativeIntoFidl};
    use cm_util::TaskGroup;
    use fidl::endpoints::{create_endpoints, create_proxy};
    use fidl::persist;
    use fuchsia_async::Task;
    use fuchsia_fs::directory::open_in_namespace;
    use routing::bedrock::structured_dict::ComponentInput;
    use std::sync::Weak;
    use vfs::directory::entry::OpenRequest;
    use vfs::directory::entry_container::Directory;
    use vfs::execution_scope::ExecutionScope;
    use vfs::file::vmo::read_only;
    use vfs::path::Path as VfsPath;
    use vfs::{pseudo_directory, ToObjectRequest};
    use {fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_data as fdata};

    fn serve_vfs_dir(root: Arc<impl Directory>) -> (Task<()>, fio::DirectoryProxy) {
        let fs_scope = ExecutionScope::new();
        let (client, server) = create_proxy::<fio::DirectoryMarker>().unwrap();
        root.open(
            fs_scope.clone(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
            VfsPath::dot(),
            ServerEnd::new(server.into_channel()),
        );

        let vfs_task = Task::spawn(async move { fs_scope.wait().await });

        (vfs_task, client)
    }

    #[fuchsia::test]
    async fn hello_world_test() -> Result<(), Error> {
        let bootfs = open_in_namespace("/pkg", fio::PERM_READABLE | fio::PERM_EXECUTABLE).unwrap();
        let resolver = FuchsiaBootResolver::new_from_directory(bootfs).await.unwrap();

        let url = "fuchsia-boot:///#meta/hello-world-rust.cm".parse().unwrap();
        let component = resolver.resolve(&ComponentAddress::from_absolute_url(&url)?).await?;

        // Check that both the returned component manifest and the component manifest in
        // the returned package dir match the expected value. This also tests that
        // the resolver returned the right package dir.
        let ResolvedComponent { resolved_url, decl, package, abi_revision, .. } = component;
        assert_eq!(url, resolved_url);
        version_history_data::HISTORY
            .check_abi_revision_for_runtime(
                abi_revision.expect("boot component should present ABI revision"),
            )
            .expect("ABI revision should be supported for boot component");

        let expected_program = Some(cm_rust::ProgramDecl {
            runner: Some("elf".parse().unwrap()),
            info: fdata::Dictionary {
                entries: Some(vec![
                    fdata::DictionaryEntry {
                        key: "binary".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str(
                            "bin/hello_world_rust".to_string(),
                        ))),
                    },
                    fdata::DictionaryEntry {
                        key: "forward_stderr_to".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str("log".to_string()))),
                    },
                    fdata::DictionaryEntry {
                        key: "forward_stdout_to".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str("log".to_string()))),
                    },
                ]),
                ..Default::default()
            },
        });

        // no need to check full decl as we just want to make
        // sure that we were able to resolve.
        assert_eq!(decl.program, expected_program);

        let ResolvedPackage { url: package_url, directory: package_dir, .. } = package.unwrap();
        assert_eq!(package_url, "fuchsia-boot:///");

        let dir_proxy = package_dir.into_proxy().unwrap();
        let path = "meta/hello-world-rust.cm";
        let file_proxy =
            fuchsia_fs::directory::open_file_async(&dir_proxy, path, fio::PERM_READABLE)
                .expect("could not open cm");

        let decl = fuchsia_fs::file::read_fidl::<fdecl::Component>(&file_proxy)
            .await
            .expect("could not read cm");
        let decl = decl.fidl_into_native();

        assert_eq!(decl.program, expected_program);

        // Try to load an executable file, like a binary, reusing the library_loader helper that
        // opens with OPEN_RIGHT_EXECUTABLE and gets a VMO with VmoFlags::EXECUTE.
        library_loader::load_vmo(&dir_proxy, "bin/hello_world_rust")
            .await
            .expect("failed to open executable file");

        let url = "fuchsia-boot:///contains/a/package#meta/hello-world-rust.cm".parse().unwrap();
        let err = resolver.resolve(&ComponentAddress::from_absolute_url(&url)?).await.unwrap_err();
        assert_matches!(err, ResolverError::PackageNotFound { .. });
        Ok(())
    }

    #[fuchsia::test]
    async fn capability_provider_test() {
        // Create a CapabilityProvider to serve fuchsia boot resolver requests.
        let bootfs = open_in_namespace("/pkg", fio::PERM_READABLE | fio::PERM_EXECUTABLE).unwrap();
        let resolver = FuchsiaBootResolver::new_from_directory(bootfs).await.unwrap();
        let resolver_provider =
            Box::new(ComponentResolverCapabilityProvider::new(resolver.clone()));
        let (client_channel, server_channel) = create_endpoints::<fresolution::ResolverMarker>();
        let task_group = TaskGroup::new();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_channel);
        resolver_provider
            .open(
                task_group.clone(),
                OpenRequest::new(
                    scope.clone(),
                    fio::OpenFlags::empty(),
                    VfsPath::dot(),
                    &mut object_request,
                ),
            )
            .await
            .expect("failed to open capability");
        // Create a client-side resolver proxy to submit resolve requests with.
        let resolver_proxy =
            client_channel.into_proxy().expect("failed converting endpoint into proxy");
        // Test that the client resolve request is served by the CapabilityProvider successfully.
        assert!(resolver_proxy.resolve("fuchsia-boot:///#meta/hello-world-rust.cm").await.is_ok());
    }

    #[fuchsia::test]
    async fn config_works() {
        let fake_checksum = cm_rust::ConfigChecksum::Sha256([0; 32]);
        let manifest = fdecl::Component {
            config: Some(
                cm_rust::ConfigDecl {
                    value_source: cm_rust::ConfigValueSource::PackagePath(
                        "meta/has_config.cvf".to_string(),
                    ),
                    fields: vec![cm_rust::ConfigField {
                        key: "foo".to_string(),
                        type_: cm_rust::ConfigValueType::String { max_size: 100 },
                        mutability: Default::default(),
                    }],
                    checksum: fake_checksum.clone(),
                }
                .native_into_fidl(),
            ),
            ..Default::default()
        };
        let values_data = fdecl::ConfigValuesData {
            values: Some(vec![fdecl::ConfigValueSpec {
                value: Some(fdecl::ConfigValue::Single(fdecl::ConfigSingleValue::String(
                    "hello, world!".to_string(),
                ))),
                ..Default::default()
            }]),
            checksum: Some(fake_checksum.clone().native_into_fidl()),
            ..Default::default()
        };
        let manifest_encoded = persist(&manifest).unwrap();
        let values_data_encoded = persist(&values_data).unwrap();
        let root = pseudo_directory! {
            "meta" => pseudo_directory! {
                "has_config.cm" => read_only(manifest_encoded),
                "has_config.cvf" => read_only(values_data_encoded),
            }
        };
        let (_task, bootfs) = serve_vfs_dir(root);
        let resolver = FuchsiaBootResolver::new_from_directory(bootfs).await.unwrap();

        let url = "fuchsia-boot:///#meta/has_config.cm".parse().unwrap();
        let component =
            resolver.resolve(&ComponentAddress::from_absolute_url(&url).unwrap()).await.unwrap();

        let ResolvedComponent { resolved_url, decl, config_values, .. } = component;
        assert_eq!(url, resolved_url);

        let config_decl = decl.config.unwrap();
        let config_values = config_values.unwrap();

        let observed_fields =
            config_encoder::ConfigFields::resolve(&config_decl, config_values, None).unwrap();
        let expected_fields = config_encoder::ConfigFields {
            fields: vec![config_encoder::ConfigField {
                key: "foo".to_string(),
                value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::String(
                    "hello, world!".to_string(),
                )),
                mutability: Default::default(),
            }],
            checksum: fake_checksum,
        };
        assert_eq!(observed_fields, expected_fields);
    }

    #[fuchsia::test]
    async fn config_requires_values() {
        let manifest = fdecl::Component {
            config: Some(
                cm_rust::ConfigDecl {
                    value_source: cm_rust::ConfigValueSource::PackagePath(
                        "meta/has_config.cvf".to_string(),
                    ),
                    fields: vec![cm_rust::ConfigField {
                        key: "foo".to_string(),
                        type_: cm_rust::ConfigValueType::String { max_size: 100 },
                        mutability: Default::default(),
                    }],
                    checksum: cm_rust::ConfigChecksum::Sha256([0; 32]),
                }
                .native_into_fidl(),
            ),
            ..Default::default()
        };
        let manifest_encoded = persist(&manifest).unwrap();
        let root = pseudo_directory! {
            "meta" => pseudo_directory! {
                "has_config.cm" => read_only(manifest_encoded),
            }
        };
        let (_task, bootfs) = serve_vfs_dir(root);
        let resolver = FuchsiaBootResolver::new_from_directory(bootfs).await.unwrap();

        let root = ComponentInstance::new_root(
            ComponentInput::default(),
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-boot:///#meta/root.cm".parse().unwrap(),
        )
        .await;

        let url = "fuchsia-boot:///#meta/has_config.cm".parse().unwrap();
        let err = resolver
            .resolve(&ComponentAddress::from_url(&url, &root).await.unwrap())
            .await
            .unwrap_err();
        assert_matches!(err, ResolverError::ConfigValuesIo { .. });
    }

    #[fuchsia::test]
    async fn resolve_errors_test() {
        let manifest_encoded = persist(&fdecl::Component {
            program: Some(fdecl::Program {
                runner: None,
                info: Some(fdata::Dictionary { entries: Some(vec![]), ..Default::default() }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
        let root = pseudo_directory! {
            "meta" => pseudo_directory! {
                // Provide a cm that will fail due to a missing runner.
                "invalid.cm" => read_only(manifest_encoded),
            },
        };
        let (_task, bootfs) = serve_vfs_dir(root);
        let resolver = FuchsiaBootResolver::new_from_directory(bootfs).await.unwrap();
        let root = ComponentInstance::new_root(
            ComponentInput::default(),
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-boot:///#meta/root.cm".parse().unwrap(),
        )
        .await;
        let url = "fuchsia-boot:///#meta/invalid.cm".parse().unwrap();
        let res = resolver.resolve(&ComponentAddress::from_url(&url, &root).await.unwrap()).await;
        assert_matches!(res, Err(ResolverError::ManifestInvalid { .. }));
    }
}
