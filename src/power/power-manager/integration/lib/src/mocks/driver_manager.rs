// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::mocks::temperature_driver::MockTemperatureDriver,
    fidl_fuchsia_io as fio,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::LocalComponentHandles,
    futures::StreamExt as _,
    std::sync::Arc,
    tracing::*,
    vfs::{
        directory::{
            entry::DirectoryEntry as _,
            helper::DirectlyMutable as _,
            immutable::simple::{simple as simple_mutable_dir, Simple as SimpleMutableDir},
        },
        name::Name,
    },
};

/// Mocks the Driver Manager to be used in integration tests.
pub struct MockDriverManager {
    devfs: Arc<vfs::directory::immutable::Simple>,
}

impl MockDriverManager {
    pub fn new() -> Arc<MockDriverManager> {
        Arc::new(Self { devfs: simple_mutable_dir() })
    }

    /// Adds a MockTemperatureDriver to the devfs maintained by this mock.
    pub fn add_temperature_mock(&self, path: &str, mock: Arc<MockTemperatureDriver>) {
        let mut root = self.devfs.clone();
        let path = path.strip_prefix("/dev/").expect("Driver paths should start with /dev/");

        let (parent_path, device_name) = {
            let path = std::path::Path::new(&path);
            let file_name = path
                .file_name()
                .expect("path does not end in a normal file or directory name")
                .to_str()
                .expect("invalid file name");
            let parent = path.parent().expect("path terminates in a root");
            info!("parent={:?}, file_name={:?}", parent, file_name);
            (parent, file_name)
        };

        for component in parent_path.components() {
            let component = component.as_os_str().to_str().expect("invalid path component");
            let component = Name::from(component).expect("invalid path component");
            root = root
                .get_or_insert(component, vfs::directory::immutable::simple::simple)
                .into_any()
                .downcast::<SimpleMutableDir>()
                .unwrap();
        }

        root.add_entry(device_name, mock.vfs_service()).unwrap();
    }

    /// Runs the mock using the provided `LocalComponentHandles`.
    ///
    /// Expected usage is to call this function from a closure for the
    /// `local_component_implementation` parameter to `RealmBuilder.add_local_child`.
    ///
    /// For example:
    ///     let mock_driver_manager = MockDriverManager::new();
    ///     let driver_manager_child = realm_builder
    ///         .add_local_child(
    ///             "driver_manager",
    ///             move |handles| Box::pin(mock_driver_manager.clone().run(handles)),
    ///             ChildOptions::new().eager(),
    ///         )
    ///         .await
    ///         .unwrap();
    ///
    pub async fn run(self: Arc<Self>, handles: LocalComponentHandles) -> Result<(), anyhow::Error> {
        info!("MockDriverManager: run");

        let scope = vfs::execution_scope::ExecutionScope::new();
        let (client, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
        let () = self.devfs.clone().open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            server,
        );
        let mut fs = ServiceFs::new();
        let _: &mut ServiceFs<_> = fs.add_remote_node("dev", client);
        fs.serve_connection(handles.outgoing_dir).unwrap();
        fs.collect::<()>().await;

        Ok(())
    }
}
