// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::marker::PhantomData;

use anyhow::{anyhow, Context, Error};
use cm_types::{IterablePath, RelativePath};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy, ServiceMarker, ServiceProxy};
use fidl_fuchsia_io::Flags;
use fuchsia_component::client::{
    connect_to_protocol_at_dir_svc, connect_to_service_instance_at_dir_svc,
};
use fuchsia_component::directory::{AsRefDirectory, Directory};
use fuchsia_component::DEFAULT_SERVICE_INSTANCE;
use namespace::{Entry, Namespace};
use tracing::error;
use zx::Status;

/// Implements access to the incoming namespace for a driver. It provides methods
/// for accessing incoming protocols and services by either their marker or proxy
/// types, and can be used as a [`Directory`] with the functions in
/// [`fuchsia_component::client`].
pub struct Incoming(Vec<Entry>);

impl Incoming {
    /// Creates a connector to the given protocol by its marker type. This can be convenient when
    /// the compiler can't deduce the [`Proxy`] type on its own.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proxy = context.incoming.protocol_marker(fidl_fuchsia_logger::LogSinkMarker).connect()?;
    /// ```
    pub fn protocol_marker<M: DiscoverableProtocolMarker>(
        &self,
        _marker: M,
    ) -> ProtocolConnector<'_, M::Proxy> {
        ProtocolConnector(self, PhantomData)
    }

    /// Creates a connector to the given protocol by its proxy type. This can be convenient when
    /// the compiler can deduce the [`Proxy`] type on its own.
    ///
    /// # Example
    ///
    /// ```ignore
    /// struct MyProxies {
    ///     log_sink: fidl_fuchsia_logger::LogSinkProxy,
    /// }
    /// let proxies = MyProxies {
    ///     log_sink: context.incoming.protocol().connect()?;
    /// };
    /// ```
    pub fn protocol<P>(&self) -> ProtocolConnector<'_, P> {
        ProtocolConnector(self, PhantomData)
    }

    /// Creates a connector to the given service's default instance by its marker type. This can be
    /// convenient when the compiler can't deduce the [`ServiceProxy`] type on its own.
    ///
    /// See [`ServiceConnector`] for more about what you can do with the connector.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let service = context.incoming.service_marker(fidl_fuchsia_hardware_i2c::ServiceMarker).connect()?;
    /// let device = service.connect_to_device()?;
    /// ```
    pub fn service_marker<M: ServiceMarker>(&self, _marker: M) -> ServiceConnector<'_, M::Proxy> {
        ServiceConnector { incoming: self, instance: DEFAULT_SERVICE_INSTANCE, _p: PhantomData }
    }

    /// Creates a connector to the given service's default instance by its proxy type. This can be
    /// convenient when the compiler can deduce the [`ServiceProxy`] type on its own.
    ///
    /// See [`ServiceConnector`] for more about what you can do with the connector.
    ///
    /// # Example
    ///
    /// ```ignore
    /// struct MyProxies {
    ///     i2c_service: fidl_fuchsia_hardware_i2c::ServiceProxy,
    /// }
    /// let proxies = MyProxies {
    ///     i2c_service: context.incoming.service().connect()?;
    /// };
    /// ```
    pub fn service<P>(&self) -> ServiceConnector<'_, P> {
        ServiceConnector { incoming: self, instance: DEFAULT_SERVICE_INSTANCE, _p: PhantomData }
    }
}

/// A builder for connecting to a protocol in the driver's incoming namespace.
pub struct ProtocolConnector<'incoming, Proxy>(&'incoming Incoming, PhantomData<Proxy>);

impl<'a, P: Proxy> ProtocolConnector<'a, P>
where
    P::Protocol: DiscoverableProtocolMarker,
{
    /// Connects to the service instance's path in the incoming namespace. Logs and returns
    /// a [`Status::CONNECTION_REFUSED`] if the service instance couldn't be opened.
    pub fn connect(self) -> Result<P, Status> {
        connect_to_protocol_at_dir_svc::<P::Protocol>(&self.0).map_err(|e| {
            error!(
                "Failed to connect to discoverable protocol `{}`: {e}",
                P::Protocol::PROTOCOL_NAME
            );
            Status::CONNECTION_REFUSED
        })
    }
}

/// A builder for connecting to an aggregated service instance in the driver's incoming namespace.
/// By default, it will connect to the default instance, named `default`. You can override this
/// by calling [`Self::instance`].
pub struct ServiceConnector<'incoming, ServiceProxy> {
    incoming: &'incoming Incoming,
    instance: &'incoming str,
    _p: PhantomData<ServiceProxy>,
}

impl<'a, S: ServiceProxy> ServiceConnector<'a, S>
where
    S::Service: ServiceMarker,
{
    /// Overrides the instance name to connect to when [`Self::connect`] is called.
    pub fn instance(self, instance: &'a str) -> Self {
        let Self { incoming, _p, .. } = self;
        Self { incoming, instance, _p }
    }

    /// Connects to the service instance's path in the incoming namespace. Logs and returns
    /// a [`Status::CONNECTION_REFUSED`] if the service instance couldn't be opened.
    pub fn connect(self) -> Result<S, Status> {
        connect_to_service_instance_at_dir_svc::<S::Service>(self.incoming, self.instance).map_err(
            |e| {
                error!(
                    "Failed to connect to aggregated service connector `{}`, instance `{}`: {e}",
                    S::Service::SERVICE_NAME,
                    self.instance
                );
                Status::CONNECTION_REFUSED
            },
        )
    }
}

impl From<Namespace> for Incoming {
    fn from(value: Namespace) -> Self {
        Incoming(value.flatten())
    }
}

/// Returns the remainder of a prefix match of `prefix` against `self` in terms of path segments.
///
/// For example:
/// ```ignore
/// match_prefix("pkg/data", "pkg") == Some("/data")
/// match_prefix("pkg_data", "pkg") == None
/// ```
fn match_prefix(match_in: &impl IterablePath, prefix: &impl IterablePath) -> Option<RelativePath> {
    let mut my_segments = match_in.iter_segments();
    let mut prefix_segments = prefix.iter_segments();
    while let Some(prefix) = prefix_segments.next() {
        if prefix != my_segments.next()? {
            return None;
        }
    }
    if prefix_segments.next().is_some() {
        // did not match all prefix segments
        return None;
    }
    let segments = Vec::from_iter(my_segments.cloned());
    Some(RelativePath::from(segments))
}

impl Directory for Incoming {
    fn open(&self, path: &str, flags: Flags, server_end: zx::Channel) -> Result<(), Error> {
        let path = path.strip_prefix("/").unwrap_or(path);
        let path = RelativePath::new(path)?;

        for entry in &self.0 {
            if let Some(remain) = match_prefix(&path, &entry.path) {
                return entry.directory.open(
                    &format!("/{}", remain.to_string()),
                    flags,
                    server_end,
                );
            }
        }
        Err(Status::NOT_FOUND)
            .with_context(|| anyhow!("Path {path} not found in incoming namespace"))
    }
}

impl AsRefDirectory for Incoming {
    fn as_ref_directory(&self) -> &dyn Directory {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_types::NamespacePath;
    use fuchsia_async::Task;
    use fuchsia_component::server::ServiceFs;
    use futures::stream::StreamExt;

    enum IncomingServices {
        I2cDevice(fidl_fuchsia_hardware_i2c::DeviceRequestStream),
        I2cDefaultService(fidl_fuchsia_hardware_i2c::ServiceRequest),
        I2cOtherService(fidl_fuchsia_hardware_i2c::ServiceRequest),
    }

    impl IncomingServices {
        async fn handle_device_stream(
            stream: fidl_fuchsia_hardware_i2c::DeviceRequestStream,
            name: &str,
        ) {
            stream
                .for_each(|msg| async move {
                    match msg.unwrap() {
                        fidl_fuchsia_hardware_i2c::DeviceRequest::GetName { responder } => {
                            responder.send(Ok(name)).unwrap();
                        }
                        _ => unimplemented!(),
                    }
                })
                .await
        }

        async fn handle(self) {
            use fidl_fuchsia_hardware_i2c::ServiceRequest::*;
            use IncomingServices::*;
            match self {
                I2cDevice(stream) => Self::handle_device_stream(stream, "device").await,
                I2cDefaultService(Device(stream)) => {
                    Self::handle_device_stream(stream, "default").await
                }
                I2cOtherService(Device(stream)) => {
                    Self::handle_device_stream(stream, "other").await
                }
            }
        }
    }

    async fn make_incoming() -> Incoming {
        let (client, server) = fidl::endpoints::create_endpoints();
        let mut fs = ServiceFs::new();
        fs.dir("svc")
            .add_fidl_service(IncomingServices::I2cDevice)
            .add_fidl_service_instance("default", IncomingServices::I2cDefaultService)
            .add_fidl_service_instance("other", IncomingServices::I2cOtherService);
        fs.serve_connection(server).expect("error serving handle");

        Task::spawn(fs.for_each_concurrent(100, IncomingServices::handle)).detach_on_drop();

        Incoming(vec![Entry { path: NamespacePath::new("/").unwrap(), directory: client }])
    }

    #[fuchsia::test]
    async fn protocol_connect_present() -> anyhow::Result<()> {
        let incoming = make_incoming().await;
        // try a protocol that we did set up
        incoming
            .protocol_marker(fidl_fuchsia_hardware_i2c::DeviceMarker)
            .connect()?
            .get_name()
            .await?
            .unwrap();
        incoming
            .protocol::<fidl_fuchsia_hardware_i2c::DeviceProxy>()
            .connect()?
            .get_name()
            .await?
            .unwrap();
        Ok(())
    }

    #[fuchsia::test]
    async fn protocol_connect_not_present() -> anyhow::Result<()> {
        let incoming = make_incoming().await;
        // try one we didn't
        incoming
            .protocol_marker(fidl_fuchsia_hwinfo::DeviceMarker)
            .connect()?
            .get_info()
            .await
            .unwrap_err();
        incoming
            .protocol::<fidl_fuchsia_hwinfo::DeviceProxy>()
            .connect()?
            .get_info()
            .await
            .unwrap_err();
        Ok(())
    }

    #[fuchsia::test]
    async fn service_connect_default_instance() -> anyhow::Result<()> {
        let incoming = make_incoming().await;
        // try the default service instance that we did set up
        assert_eq!(
            "default",
            &incoming
                .service_marker(fidl_fuchsia_hardware_i2c::ServiceMarker)
                .connect()?
                .connect_to_device()?
                .get_name()
                .await?
                .unwrap()
        );
        assert_eq!(
            "default",
            &incoming
                .service::<fidl_fuchsia_hardware_i2c::ServiceProxy>()
                .connect()?
                .connect_to_device()?
                .get_name()
                .await?
                .unwrap()
        );
        Ok(())
    }

    #[fuchsia::test]
    async fn service_connect_other_instance() -> anyhow::Result<()> {
        let incoming = make_incoming().await;
        // try the other service instance that we did set up
        assert_eq!(
            "other",
            &incoming
                .service_marker(fidl_fuchsia_hardware_i2c::ServiceMarker)
                .instance("other")
                .connect()?
                .connect_to_device()?
                .get_name()
                .await?
                .unwrap()
        );
        assert_eq!(
            "other",
            &incoming
                .service::<fidl_fuchsia_hardware_i2c::ServiceProxy>()
                .instance("other")
                .connect()?
                .connect_to_device()?
                .get_name()
                .await?
                .unwrap()
        );
        Ok(())
    }

    #[fuchsia::test]
    async fn service_connect_invalid_instance() -> anyhow::Result<()> {
        let incoming = make_incoming().await;
        // try the invalid service instance that we did not set up
        incoming
            .service_marker(fidl_fuchsia_hardware_i2c::ServiceMarker)
            .instance("invalid")
            .connect()?
            .connect_to_device()?
            .get_name()
            .await
            .unwrap_err();
        incoming
            .service::<fidl_fuchsia_hardware_i2c::ServiceProxy>()
            .instance("invalid")
            .connect()?
            .connect_to_device()?
            .get_name()
            .await
            .unwrap_err();
        Ok(())
    }
}
