// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_hardware_temperature as ftemperature, fuchsia_zircon as zx, futures::TryStreamExt,
    parking_lot::RwLock, std::sync::Arc, tracing::*,
};

/// Mocks a temperature driver to be used in integration tests.
pub struct MockTemperatureDriver {
    name: String,
    current_temperature: Arc<RwLock<f32>>,
}

impl MockTemperatureDriver {
    pub fn new(name: &str) -> Arc<MockTemperatureDriver> {
        Arc::new(Self { name: name.to_string(), current_temperature: Arc::new(RwLock::new(0.0)) })
    }

    /// Returns the mock's service as a vfs::Service.
    ///
    /// Expected usage is to add the vfs::Service as an entry in a directory.
    ///
    /// For example:
    ///     let mock_temperature_driver = MockTemperatureDriver::new("mock_temperature");
    ///     let devfs = vfs::directory::immutable::simple::simple();
    ///     devfs.add_entry("mock_temperature", mock.vfs_service()).unwrap();
    ///
    pub fn vfs_service(&self) -> Arc<vfs::service::Service> {
        let name = self.name.clone();
        let current_temperature = self.current_temperature.clone();

        vfs::service::host(move |mut stream: ftemperature::DeviceRequestStream| {
            let name = name.clone();
            let current_temperature_clone = current_temperature.clone();
            async move {
                info!("MockTemperatureDriver [{}]: new connection", name);
                while let Some(ftemperature::DeviceRequest::GetTemperatureCelsius { responder }) =
                    stream.try_next().await.unwrap()
                {
                    info!(
                        "MockTemperatureDriver [{}]: received temperature request; sending {}",
                        name,
                        *current_temperature_clone.read()
                    );

                    let _ = responder
                        .send(zx::Status::OK.into_raw(), *current_temperature_clone.read());
                }

                info!("MockTemperatureDriver [{}]: closing connection", name);
            }
        })
    }

    pub fn set_temperature(&self, celsius: f32) {
        info!("MockTemperatureDriver [{}]: setting mock temperature to {}", self.name, celsius);
        *self.current_temperature.write() = celsius;
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, fidl::endpoints::Proxy, fidl_fuchsia_io as fio,
        fuchsia_component::client::connect_to_named_protocol_at_dir_root,
        vfs::directory::entry::DirectoryEntry, vfs::pseudo_directory,
    };

    #[fuchsia::test]
    async fn test_set_temperature() {
        let mock = MockTemperatureDriver::new("mock");

        // Set up the fake directory to serve the mock driver
        let pseudo_dir = pseudo_directory! {
            "mock" => mock.vfs_service()
        };
        let (dir, dir_server) = fidl::endpoints::create_proxy().unwrap();
        pseudo_dir.open(
            vfs::execution_scope::ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            dir_server,
        );

        let driver_proxy = connect_to_named_protocol_at_dir_root::<ftemperature::DeviceMarker>(
            &fio::DirectoryProxy::from_channel(dir.into_channel().unwrap()),
            "mock",
        )
        .expect("Failed to connect to the mock temperature device");

        // Default temperature is initially 0
        assert_eq!(driver_proxy.get_temperature_celsius().await.unwrap(), (0, 0.0));

        // Set a new temperature and verify the client gets the updated value
        mock.set_temperature(45.67);
        assert_eq!(driver_proxy.get_temperature_celsius().await.unwrap(), (0, 45.67));
    }
}
