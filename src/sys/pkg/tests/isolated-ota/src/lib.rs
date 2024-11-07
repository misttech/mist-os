// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use assert_matches::assert_matches;
use async_trait::async_trait;
use blobfs_ramdisk::BlobfsRamdisk;
use fidl::endpoints::{RequestStream, ServerEnd};
use fidl_fuchsia_paver::{Asset, Configuration};
use fidl_fuchsia_pkg_ext::{MirrorConfigBuilder, RepositoryConfigBuilder, RepositoryConfigs};
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, Ref, Route,
};
use fuchsia_pkg_testing::{Package, PackageBuilder};
use fuchsia_sync::Mutex;
use futures::future::FutureExt;
use futures::prelude::*;
use http::uri::Uri;
use isolated_ota::{download_and_apply_update_with_updater, OmahaConfig, UpdateError};
use isolated_ota_env::{
    expose_mock_paver, OmahaState, TestEnvBuilder, TestExecutor, TestParams, GLOBAL_SSL_CERTS_PATH,
};
use isolated_swd::updater::Updater;
use mock_omaha_server::OmahaResponse;
use mock_paver::{hooks as mphooks, PaverEvent};
use pretty_assertions::assert_eq;
use std::collections::BTreeSet;
use vfs::directory::entry_container::Directory;
use vfs::file::vmo::read_only;
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

struct TestResult {
    blobfs: Option<BlobfsRamdisk>,
    expected_blobfs_contents: BTreeSet<fuchsia_hash::Hash>,
    pub paver_events: Vec<PaverEvent>,
    pub result: Result<(), UpdateError>,
}

impl TestResult {
    /// Assert that all blobs in all the packages that were part of the Update
    /// have been installed into the blobfs, and that the blobfs contains no extra blobs.
    pub fn check_packages(&self) {
        let actual_contents = self
            .blobfs
            .as_ref()
            .expect("Test had no blobfs")
            .list_blobs()
            .expect("Listing blobfs blobs");
        assert_eq!(actual_contents, self.expected_blobfs_contents);
    }
}

struct IsolatedOtaTestExecutor {}
impl IsolatedOtaTestExecutor {
    pub fn new() -> Box<Self> {
        Box::new(Self {})
    }
}

#[async_trait(?Send)]
impl TestExecutor<TestResult> for IsolatedOtaTestExecutor {
    async fn run(&self, params: TestParams) -> TestResult {
        let realm_builder = RealmBuilder::new().await.unwrap();

        let pkg_component =
            realm_builder.add_child("pkg", "#meta/pkg.cm", ChildOptions::new()).await.unwrap();
        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                    .capability(Capability::protocol_by_name(
                        "fuchsia.metrics.MetricEventLoggerFactory",
                    ))
                    .capability(Capability::protocol_by_name("fuchsia.net.name.Lookup"))
                    .capability(Capability::protocol_by_name("fuchsia.posix.socket.Provider"))
                    .capability(Capability::protocol_by_name("fuchsia.tracing.provider.Registry"))
                    .from(Ref::parent())
                    .to(&pkg_component),
            )
            .await
            .unwrap();

        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.update.installer.Installer"))
                    .from(&pkg_component)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        let directories_out_dir = vfs::pseudo_directory! {
            "config" => vfs::pseudo_directory! {
                "data" => vfs::pseudo_directory!{
                    "repositories" => vfs::remote::remote_dir(fuchsia_fs::directory::open_in_namespace(params.repo_config_dir.path().to_str().unwrap(), fio::PERM_READABLE).unwrap())
                },
                "build-info" => vfs::pseudo_directory!{
                    "build" => read_only(b"test")
                },
            "ssl" => vfs::remote::remote_dir(
                    params.ssl_certs
                ),
            },
        };
        let directories_out_dir = Mutex::new(Some(directories_out_dir));
        let directories_component = realm_builder
            .add_local_child(
                "directories",
                move |handles| {
                    let directories_out_dir = directories_out_dir
                        .lock()
                        .take()
                        .expect("mock component should only be launched once");
                    let scope = vfs::execution_scope::ExecutionScope::new();
                    directories_out_dir.open(
                        scope.clone(),
                        fio::OpenFlags::RIGHT_READABLE
                            | fio::OpenFlags::RIGHT_WRITABLE
                            | fio::OpenFlags::RIGHT_EXECUTABLE,
                        vfs::path::Path::dot(),
                        handles.outgoing_dir.into_channel().into(),
                    );
                    async move {
                        scope.wait().await;
                        Ok(())
                    }
                    .boxed()
                },
                ChildOptions::new(),
            )
            .await
            .unwrap();

        let paver_dir_proxy = params
            .paver_connector
            .into_proxy()
            .expect("failed to convert paver dir client end to proxy");
        let paver_child = realm_builder
            .add_local_child(
                "paver",
                move |handles: LocalComponentHandles| {
                    expose_mock_paver(
                        handles,
                        fuchsia_fs::directory::clone_no_describe(&paver_dir_proxy, None).unwrap(),
                    )
                    .boxed()
                },
                ChildOptions::new().eager(),
            )
            .await
            .expect("failed to add paver child");

        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.paver.Paver"))
                    .from(&paver_child)
                    .to(&pkg_component),
            )
            .await
            .unwrap();

        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.paver.Paver"))
                    .from(&paver_child)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        // Directory routes
        realm_builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("config-data")
                            .path("/config/data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .from(&directories_component)
                    .to(&pkg_component),
            )
            .await
            .unwrap();
        realm_builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("root-ssl-certificates")
                            .path(GLOBAL_SSL_CERTS_PATH)
                            .rights(fio::R_STAR_DIR),
                    )
                    .from(&directories_component)
                    .to(&pkg_component),
            )
            .await
            .unwrap();
        realm_builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("build-info")
                            .rights(fio::R_STAR_DIR)
                            .path("/config/build-info"),
                    )
                    .from(&directories_component)
                    .to(&pkg_component),
            )
            .await
            .unwrap();

        let (blobfs_ramdisk, blobfs_handle) = match params.blobfs {
            Some(blobfs_handle) => (None, blobfs_handle),
            None => {
                let blobfs_ramdisk = BlobfsRamdisk::start().await.expect("launching blobfs");
                let blobfs_handle =
                    blobfs_ramdisk.root_dir_handle().expect("getting blobfs root handle");
                (Some(blobfs_ramdisk), blobfs_handle)
            }
        };

        let blobfs_proxy = blobfs_handle.into_proxy().unwrap();
        let (blobfs_client_end_clone, remote) =
            fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        blobfs_proxy
            .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, remote.into_channel().into())
            .unwrap();

        let blobfs_proxy_clone = blobfs_client_end_clone.into_proxy().unwrap();
        let blobfs_vfs = vfs::remote::remote_dir(blobfs_proxy_clone);
        let blobfs_reflector = realm_builder
            .add_local_child(
                "pkg_cache_blobfs",
                move |handles| {
                    let blobfs_vfs = blobfs_vfs.clone();
                    let out_dir = vfs::pseudo_directory! {
                        "blob" => blobfs_vfs,
                    };
                    let scope = vfs::execution_scope::ExecutionScope::new();
                    out_dir.open(
                        scope.clone(),
                        fio::OpenFlags::RIGHT_READABLE
                            | fio::OpenFlags::RIGHT_WRITABLE
                            | fio::OpenFlags::RIGHT_EXECUTABLE,
                        vfs::path::Path::dot(),
                        handles.outgoing_dir.into_channel().into(),
                    );
                    async move {
                        scope.wait().await;
                        Ok(())
                    }
                    .boxed()
                },
                ChildOptions::new(),
            )
            .await
            .unwrap();

        realm_builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("blob-exec")
                            .path("/blob")
                            .rights(fio::RW_STAR_DIR | fio::Operations::EXECUTE),
                    )
                    .from(&blobfs_reflector)
                    .to(&pkg_component),
            )
            .await
            .unwrap();

        let channel_clone = params.channel.clone();

        let realm_instance = realm_builder.build().await.unwrap();

        let installer_proxy = realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<fidl_fuchsia_update_installer::InstallerMarker>()
            .expect("connect to system updater");
        let paver_proxy = realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<fidl_fuchsia_paver::PaverMarker>()
            .expect("connect to paver");

        let updater = Updater::new_with_proxies(installer_proxy, paver_proxy);

        let result = download_and_apply_update_with_updater(
            updater,
            &channel_clone,
            &params.version,
            params.update_url_source,
        )
        .await;

        TestResult {
            blobfs: blobfs_ramdisk,
            expected_blobfs_contents: params.expected_blobfs_contents,
            paver_events: params.paver.take_events(),
            result,
        }
    }
}

async fn build_test_package() -> Result<Package, Error> {
    PackageBuilder::new("test-package")
        .add_resource_at("data/test", "hello, world!".as_bytes())
        .build()
        .await
        .context("Building test package")
}

#[fasync::run_singlethreaded(test)]
pub async fn test_no_network() -> Result<(), Error> {
    // Test what happens when we can't reach the remote repo.
    let bad_mirror =
        MirrorConfigBuilder::new("http://does-not-exist.fuchsia.com".parse::<Uri>().unwrap())?
            .build();
    let invalid_repo = RepositoryConfigs::Version1(vec![RepositoryConfigBuilder::new(
        fuchsia_url::RepositoryUrl::parse_host("fuchsia.com".to_owned()).unwrap(),
    )
    .add_mirror(bad_mirror)
    .build()]);

    let env = TestEnvBuilder::new()
        .test_executor(IsolatedOtaTestExecutor::new())
        .repo_config(invalid_repo)
        .build()
        .await
        .context("Building TestEnv")?;

    let update_result = env.run().await;
    assert_eq!(
        update_result.paver_events,
        vec![
            PaverEvent::QueryCurrentConfiguration,
            PaverEvent::ReadAsset {
                configuration: Configuration::A,
                asset: Asset::VerifiedBootMetadata
            },
            PaverEvent::ReadAsset { configuration: Configuration::A, asset: Asset::Kernel },
            PaverEvent::QueryCurrentConfiguration,
            PaverEvent::QueryConfigurationStatus { configuration: Configuration::A },
            PaverEvent::SetConfigurationUnbootable { configuration: Configuration::B },
            PaverEvent::BootManagerFlush,
        ]
    );
    update_result.check_packages();

    let err = update_result.result.unwrap_err();
    assert_matches!(err, UpdateError::InstallError(_));
    Ok(())
}

#[fasync::run_singlethreaded(test)]
pub async fn test_pave_fails() -> Result<(), Error> {
    // Test what happens if the paver fails while paving.
    let test_package = build_test_package().await?;
    let paver_hook = |p: &PaverEvent| {
        if let PaverEvent::WriteAsset { payload, .. } = p {
            if payload.as_slice() == b"zbi-contents" {
                return zx::Status::IO;
            }
        }
        zx::Status::OK
    };

    let env = TestEnvBuilder::new()
        .test_executor(IsolatedOtaTestExecutor::new())
        .paver(|p| p.insert_hook(mphooks::return_error(paver_hook)))
        .add_package(test_package)
        .fuchsia_image(b"zbi-contents".to_vec(), None)
        .build()
        .await
        .context("Building TestEnv")?;

    let result = env.run().await;
    assert_eq!(
        result.paver_events,
        vec![
            PaverEvent::QueryCurrentConfiguration,
            PaverEvent::ReadAsset {
                configuration: Configuration::A,
                asset: Asset::VerifiedBootMetadata
            },
            PaverEvent::ReadAsset { configuration: Configuration::A, asset: Asset::Kernel },
            PaverEvent::QueryCurrentConfiguration,
            PaverEvent::QueryConfigurationStatus { configuration: Configuration::A },
            PaverEvent::SetConfigurationUnbootable { configuration: Configuration::B },
            PaverEvent::BootManagerFlush,
            PaverEvent::ReadAsset { configuration: Configuration::B, asset: Asset::Kernel },
            PaverEvent::ReadAsset { configuration: Configuration::A, asset: Asset::Kernel },
            PaverEvent::WriteAsset {
                asset: Asset::Kernel,
                configuration: Configuration::B,
                payload: b"zbi-contents".to_vec(),
            },
        ]
    );
    assert_matches!(result.result.unwrap_err(), UpdateError::InstallError(_));

    Ok(())
}

#[fasync::run_singlethreaded(test)]
pub async fn test_updater_succeeds() -> Result<(), Error> {
    let mut builder = TestEnvBuilder::new()
        .test_executor(IsolatedOtaTestExecutor::new())
        .fuchsia_image(b"zbi-contents".to_vec(), Some(b"vbmeta-contents".to_vec()))
        .recovery_image(
            b"recovery-zbi-contents".to_vec(),
            Some(b"recovery-vbmeta-contents".to_vec()),
        )
        .firmware_image("".into(), b"This is a bootloader upgrade".to_vec())
        .firmware_image("test".into(), b"This is the test firmware".to_vec());
    for i in 0i64..3 {
        let name = format!("test-package{i}");
        let package = PackageBuilder::new(name)
            .add_resource_at(
                format!("data/my-package-data-{i}"),
                format!("This is some test data for test package {i}").as_bytes(),
            )
            .add_resource_at("bin/binary", "#!/boot/bin/sh\necho Hello".as_bytes())
            .build()
            .await
            .context("Building test package")?;
        builder = builder.add_package(package);
    }

    let env = builder.build().await.context("Building TestEnv")?;
    let result = env.run().await;

    result.check_packages();
    assert!(result.result.is_ok());
    assert_eq!(
        result.paver_events,
        vec![
            PaverEvent::QueryCurrentConfiguration,
            PaverEvent::ReadAsset {
                configuration: Configuration::A,
                asset: Asset::VerifiedBootMetadata
            },
            PaverEvent::ReadAsset { configuration: Configuration::A, asset: Asset::Kernel },
            PaverEvent::QueryCurrentConfiguration,
            PaverEvent::QueryConfigurationStatus { configuration: Configuration::A },
            PaverEvent::SetConfigurationUnbootable { configuration: Configuration::B },
            PaverEvent::BootManagerFlush,
            PaverEvent::ReadAsset { configuration: Configuration::B, asset: Asset::Kernel },
            PaverEvent::ReadAsset { configuration: Configuration::A, asset: Asset::Kernel },
            PaverEvent::ReadAsset {
                configuration: Configuration::B,
                asset: Asset::VerifiedBootMetadata
            },
            PaverEvent::ReadAsset {
                configuration: Configuration::A,
                asset: Asset::VerifiedBootMetadata
            },
            PaverEvent::ReadFirmware { configuration: Configuration::B, firmware_type: "".into() },
            PaverEvent::ReadFirmware { configuration: Configuration::A, firmware_type: "".into() },
            PaverEvent::ReadFirmware {
                configuration: Configuration::B,
                firmware_type: "test".into()
            },
            PaverEvent::ReadFirmware {
                configuration: Configuration::A,
                firmware_type: "test".into()
            },
            PaverEvent::WriteFirmware {
                configuration: Configuration::B,
                firmware_type: "".into(),
                payload: b"This is a bootloader upgrade".into(),
            },
            PaverEvent::WriteFirmware {
                configuration: Configuration::B,
                firmware_type: "test".into(),
                payload: b"This is the test firmware".into(),
            },
            PaverEvent::WriteAsset {
                asset: Asset::Kernel,
                configuration: Configuration::B,
                payload: b"zbi-contents".into(),
            },
            PaverEvent::WriteAsset {
                asset: Asset::VerifiedBootMetadata,
                configuration: Configuration::B,
                payload: b"vbmeta-contents".into(),
            },
            PaverEvent::DataSinkFlush,
            // Note that recovery isn't written, as isolated-ota skips them.
            PaverEvent::SetConfigurationActive { configuration: Configuration::B },
            PaverEvent::BootManagerFlush,
            // This is the isolated-ota library checking to see if the paver configured ABR properly.
            PaverEvent::QueryActiveConfiguration,
        ]
    );
    Ok(())
}

fn launch_cloned_blobfs(
    end: ServerEnd<fio::NodeMarker>,
    flags: fio::OpenFlags,
    parent_flags: fio::OpenFlags,
) {
    let flags =
        if flags.contains(fio::OpenFlags::CLONE_SAME_RIGHTS) { parent_flags } else { flags };
    fasync::Task::spawn(async move {
        serve_failing_blobfs(end.into_stream().unwrap().cast_stream(), flags)
            .await
            .unwrap_or_else(|e| panic!("Failed to serve cloned blobfs handle: {e:?}"));
    })
    .detach();
}

async fn serve_failing_blobfs(
    mut stream: fio::DirectoryRequestStream,
    open_flags: fio::OpenFlags,
) -> Result<(), Error> {
    if open_flags.contains(fio::OpenFlags::DESCRIBE) {
        stream
            .control_handle()
            .send_on_open_(
                zx::Status::OK.into_raw(),
                Some(fio::NodeInfoDeprecated::Directory(fio::DirectoryObject)),
            )
            .context("sending on open")?;
    }
    while let Some(req) = stream.try_next().await? {
        match req {
            fio::DirectoryRequest::Clone { flags, object, control_handle: _ } => {
                launch_cloned_blobfs(object, flags, open_flags)
            }
            fio::DirectoryRequest::Clone2 { request, control_handle: _ } => launch_cloned_blobfs(
                ServerEnd::new(request.into_channel()),
                fio::OpenFlags::CLONE_SAME_RIGHTS,
                open_flags & fio::OPEN_RIGHTS,
            ),
            fio::DirectoryRequest::Close { responder } => {
                responder.send(Err(zx::Status::IO.into_raw())).context("failing close")?
            }
            fio::DirectoryRequest::GetConnectionInfo { responder } => {
                let _ = responder;
                todo!("https://fxbug.dev/324112547");
            }
            fio::DirectoryRequest::Sync { responder } => {
                responder.send(Err(zx::Status::IO.into_raw())).context("failing sync")?
            }
            fio::DirectoryRequest::AdvisoryLock { request: _, responder } => {
                responder.send(Err(zx::sys::ZX_ERR_NOT_SUPPORTED))?
            }
            fio::DirectoryRequest::GetAttr { responder } => responder
                .send(
                    zx::Status::IO.into_raw(),
                    &fio::NodeAttributes {
                        mode: 0,
                        id: 0,
                        content_size: 0,
                        storage_size: 0,
                        link_count: 0,
                        creation_time: 0,
                        modification_time: 0,
                    },
                )
                .context("failing getattr")?,
            fio::DirectoryRequest::SetAttr { flags: _, attributes: _, responder } => {
                responder.send(zx::Status::IO.into_raw()).context("failing setattr")?
            }
            fio::DirectoryRequest::GetAttributes { query, responder } => {
                let _ = responder;
                todo!("https://fxbug.dev/324112547: query={:?}", query);
            }
            fio::DirectoryRequest::UpdateAttributes { payload, responder } => {
                let _ = responder;
                todo!("https://fxbug.dev/324112547: payload={:?}", payload);
            }
            fio::DirectoryRequest::ListExtendedAttributes { iterator: _, control_handle: _ } => {
                todo!("https://fxbug.dev/42073111");
            }
            fio::DirectoryRequest::GetExtendedAttribute { name, responder: _ } => {
                todo!("https://fxbug.dev/42073111: name={:?}", name);
            }
            fio::DirectoryRequest::SetExtendedAttribute { name, value, mode, responder: _ } => {
                todo!(
                    "https://fxbug.dev/42073111: name={:?} value={:?} mode={:?}",
                    name,
                    value,
                    mode
                );
            }
            fio::DirectoryRequest::RemoveExtendedAttribute { name, responder: _ } => {
                todo!("https://fxbug.dev/42073111: name={:?}", name);
            }
            fio::DirectoryRequest::GetFlags { responder } => responder
                .send(zx::Status::IO.into_raw(), fio::OpenFlags::empty())
                .context("failing getflags")?,
            fio::DirectoryRequest::SetFlags { flags: _, responder } => {
                responder.send(zx::Status::IO.into_raw()).context("failing setflags")?
            }
            fio::DirectoryRequest::Open { flags, mode: _, path, object, control_handle: _ } => {
                if &path == "." {
                    launch_cloned_blobfs(object, flags, open_flags);
                } else {
                    object.close_with_epitaph(zx::Status::IO).context("failing open")?;
                }
            }
            fio::DirectoryRequest::Open2 { path, protocols, object_request, control_handle: _ } => {
                let _ = object_request;
                todo!("https://fxbug.dev/293947862: path={} protocols={:?}", path, protocols);
            }
            fio::DirectoryRequest::Open3 { path, flags, options, object, control_handle: _ } => {
                vfs::ObjectRequest::new3(flags, &options, object).handle(|request| {
                    if path == "." {
                        let mut open1_flags = fio::OpenFlags::empty();
                        if flags.contains(fio::PERM_READABLE) {
                            open1_flags |= fio::OpenFlags::RIGHT_READABLE;
                        }
                        if flags.contains(fio::PERM_WRITABLE) {
                            open1_flags |= fio::OpenFlags::RIGHT_WRITABLE;
                        }
                        if flags.contains(fio::PERM_EXECUTABLE) {
                            open1_flags |= fio::OpenFlags::RIGHT_EXECUTABLE;
                        }

                        launch_cloned_blobfs(
                            request.take().into_server_end(),
                            open1_flags,
                            open_flags,
                        );
                        Ok(())
                    } else {
                        Err(zx::Status::IO)
                    }
                });
            }
            fio::DirectoryRequest::Unlink { name: _, options: _, responder } => {
                responder.send(Err(zx::Status::IO.into_raw())).context("failing unlink")?
            }
            fio::DirectoryRequest::ReadDirents { max_bytes: _, responder } => {
                responder.send(zx::Status::IO.into_raw(), &[]).context("failing readdirents")?
            }
            fio::DirectoryRequest::Rewind { responder } => {
                responder.send(zx::Status::IO.into_raw()).context("failing rewind")?
            }
            fio::DirectoryRequest::GetToken { responder } => {
                responder.send(zx::Status::IO.into_raw(), None).context("failing gettoken")?
            }
            fio::DirectoryRequest::Rename { src: _, dst_parent_token: _, dst: _, responder } => {
                responder.send(Err(zx::Status::IO.into_raw())).context("failing rename")?
            }
            fio::DirectoryRequest::Link { src: _, dst_parent_token: _, dst: _, responder } => {
                responder.send(zx::Status::IO.into_raw()).context("failing link")?
            }
            fio::DirectoryRequest::Watch { mask: _, options: _, watcher: _, responder } => {
                responder.send(zx::Status::IO.into_raw()).context("failing watch")?
            }
            fio::DirectoryRequest::Query { responder } => {
                responder.send(fio::DIRECTORY_PROTOCOL_NAME.as_bytes())?;
            }
            fio::DirectoryRequest::QueryFilesystem { responder } => responder
                .send(zx::Status::IO.into_raw(), None)
                .context("failing queryfilesystem")?,
            fio::DirectoryRequest::CreateSymlink { responder, .. } => {
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?
            }
            fio::DirectoryRequest::_UnknownMethod { .. } => (),
        };
    }

    Ok(())
}

#[fasync::run_singlethreaded(test)]
pub async fn test_blobfs_broken() -> Result<(), Error> {
    let (client, server) = fidl::endpoints::create_request_stream().unwrap();
    let package = build_test_package().await?;
    let env = TestEnvBuilder::new()
        .test_executor(IsolatedOtaTestExecutor::new())
        .add_package(package)
        .fuchsia_image(b"zbi-contents".to_vec(), None)
        .blobfs(client)
        .build()
        .await
        .context("Building TestEnv")?;

    fasync::Task::spawn(async move {
        serve_failing_blobfs(server, fio::OpenFlags::empty())
            .await
            .unwrap_or_else(|e| panic!("Failed to serve blobfs: {e:?}"));
    })
    .detach();

    let result = env.run().await;

    assert_matches!(result.result, Err(UpdateError::InstallError(_)));

    Ok(())
}

#[fasync::run_singlethreaded(test)]
pub async fn test_omaha_broken() -> Result<(), Error> {
    let bad_omaha_config = OmahaConfig {
        app_id: "broken-omaha-test".to_owned(),
        server_url: "http://does-not-exist.fuchsia.com".to_owned(),
    };
    let package = build_test_package().await?;
    let env = TestEnvBuilder::new()
        .test_executor(IsolatedOtaTestExecutor::new())
        .add_package(package)
        .fuchsia_image(b"zbi-contents".to_vec(), None)
        .omaha_state(OmahaState::Manual(bad_omaha_config))
        .build()
        .await
        .context("Building TestEnv")?;

    let result = env.run().await;
    assert_matches!(result.result, Err(UpdateError::InstallError(_)));

    Ok(())
}

#[fasync::run_singlethreaded(test)]
pub async fn test_omaha_works() -> Result<(), Error> {
    let mut builder = TestEnvBuilder::new()
        .test_executor(IsolatedOtaTestExecutor::new())
        .fuchsia_image(b"zbi-contents".to_vec(), None)
        .recovery_image(
            b"recovery-zbi-contents".to_vec(),
            Some(b"recovery-vbmeta-contents".to_vec()),
        )
        .firmware_image("".into(), b"This is a bootloader upgrade".to_vec())
        .firmware_image("test".into(), b"This is the test firmware".to_vec());
    for i in 0i64..3 {
        let name = format!("test-package{i}");
        let package = PackageBuilder::new(name)
            .add_resource_at(
                format!("data/my-package-data-{i}"),
                format!("This is some test data for test package {i}").as_bytes(),
            )
            .add_resource_at("bin/binary", "#!/boot/bin/sh\necho Hello".as_bytes())
            .build()
            .await
            .context("Building test package")?;
        builder = builder.add_package(package);
    }

    let env = builder
        .omaha_state(OmahaState::Auto(OmahaResponse::Update))
        .build()
        .await
        .context("Building TestEnv")?;

    let result = env.run().await;
    assert_eq!(
        result.paver_events,
        vec![
            PaverEvent::QueryCurrentConfiguration,
            PaverEvent::ReadAsset {
                configuration: Configuration::A,
                asset: Asset::VerifiedBootMetadata
            },
            PaverEvent::ReadAsset { configuration: Configuration::A, asset: Asset::Kernel },
            PaverEvent::QueryCurrentConfiguration,
            PaverEvent::QueryConfigurationStatus { configuration: Configuration::A },
            PaverEvent::SetConfigurationUnbootable { configuration: Configuration::B },
            PaverEvent::BootManagerFlush,
            PaverEvent::ReadAsset { configuration: Configuration::B, asset: Asset::Kernel },
            PaverEvent::ReadAsset { configuration: Configuration::A, asset: Asset::Kernel },
            PaverEvent::ReadFirmware { configuration: Configuration::B, firmware_type: "".into() },
            PaverEvent::ReadFirmware { configuration: Configuration::A, firmware_type: "".into() },
            PaverEvent::ReadFirmware {
                configuration: Configuration::B,
                firmware_type: "test".into()
            },
            PaverEvent::ReadFirmware {
                configuration: Configuration::A,
                firmware_type: "test".into()
            },
            PaverEvent::WriteFirmware {
                configuration: Configuration::B,
                firmware_type: "".into(),
                payload: b"This is a bootloader upgrade".into(),
            },
            PaverEvent::WriteFirmware {
                configuration: Configuration::B,
                firmware_type: "test".into(),
                payload: b"This is the test firmware".into(),
            },
            PaverEvent::WriteAsset {
                asset: Asset::Kernel,
                configuration: Configuration::B,
                payload: b"zbi-contents".to_vec(),
            },
            PaverEvent::DataSinkFlush,
            // Note that recovery isn't written, as isolated-ota skips them.
            PaverEvent::SetConfigurationActive { configuration: Configuration::B },
            PaverEvent::BootManagerFlush,
            // This is the isolated-ota library checking to see if the paver configured ABR properly.
            PaverEvent::QueryActiveConfiguration,
        ]
    );
    assert_matches!(result.result, Ok(()));
    let () = result.check_packages();

    Ok(())
}
