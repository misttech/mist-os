// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{create_request_stream, ClientEnd, ServerEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::TryStreamExt;
use std::sync::{Arc, Mutex};
use vfs::directory::entry::DirectoryEntry;

use crate::{registry, CapabilityTrait, ConversionError};

/// A capability that vends a single Zircon handle.
#[derive(Clone, Debug)]
pub struct OneShotHandle(Arc<Mutex<Option<zx::Handle>>>);

impl OneShotHandle {
    /// Returns the handle in this [OneShotHandle], taking it out.
    ///
    /// Subsequent calls will return an `Unavailable` error.
    pub fn get_handle(&self) -> Result<zx::Handle, fsandbox::HandleCapabilityError> {
        self.0.lock().unwrap().take().ok_or(fsandbox::HandleCapabilityError::Unavailable)
    }
}

impl From<zx::Handle> for OneShotHandle {
    fn from(handle: zx::Handle) -> Self {
        OneShotHandle(Arc::new(Mutex::new(Some(handle))))
    }
}

impl OneShotHandle {
    /// Serves the `fuchsia.component.HandleCapability` protocol.
    pub(crate) async fn serve_handle_capability(
        &self,
        mut stream: fsandbox::HandleCapabilityRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                fsandbox::HandleCapabilityRequest::Clone2 { request, control_handle: _ } => {
                    // The clone is registered under the koid of the client end.
                    let koid = request.basic_info().unwrap().related_koid;
                    let server_end: ServerEnd<fsandbox::HandleCapabilityMarker> =
                        request.into_channel().into();
                    let stream = server_end.into_stream().unwrap();
                    self.clone().serve_and_register(stream, koid);
                }
                fsandbox::HandleCapabilityRequest::GetHandle { responder } => {
                    responder.send(self.get_handle())?;
                }
            }
        }

        Ok(())
    }

    /// Serves the `fuchsia.sandbox.HandleCapability` protocol for this OneShotHandle
    /// and moves it into the registry.
    fn serve_and_register(self, stream: fsandbox::HandleCapabilityRequestStream, koid: zx::Koid) {
        let one_shot = self.clone();

        // Move this capability into the registry.
        registry::spawn_task(self.into(), koid, async move {
            one_shot
                .serve_handle_capability(stream)
                .await
                .expect("failed to serve HandleCapability");
        });
    }
}

impl CapabilityTrait for OneShotHandle {
    /// Attempts to convert into a DirectoryEntry that calls `fuchsia.io.Openable/Open` on the handle.
    ///
    /// The handle must be a channel that speaks the `Openable` protocol.
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        let handle = self.get_handle().map_err(|err| ConversionError::Handle { err })?;

        let basic_info = handle.basic_info().map_err(|_| ConversionError::NotSupported)?;
        if basic_info.object_type != zx::ObjectType::CHANNEL {
            return Err(ConversionError::NotSupported);
        }

        let openable = ClientEnd::<fio::OpenableMarker>::from(handle).into_proxy().unwrap();

        Ok(vfs::service::endpoint(move |_scope, server_end| {
            // TODO(b/306037927): Calling Open on a channel that doesn't speak Openable may
            // inadvertently close the channel.
            let _ = openable.open(
                fio::OpenFlags::empty(),
                fio::ModeType::empty(),
                ".",
                server_end.into_zx_channel().into(),
            );
        }))
    }
}

impl From<OneShotHandle> for ClientEnd<fsandbox::HandleCapabilityMarker> {
    /// Serves the `fuchsia.sandbox.HandleCapability` protocol for this OneShotHandle
    /// and moves it into the registry.
    fn from(one_shot: OneShotHandle) -> Self {
        let (client_end, stream) =
            create_request_stream::<fsandbox::HandleCapabilityMarker>().unwrap();
        one_shot.serve_and_register(stream, client_end.get_koid().unwrap());
        client_end
    }
}

impl From<OneShotHandle> for fsandbox::Capability {
    fn from(one_shot: OneShotHandle) -> Self {
        Self::Handle(one_shot.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Capability;
    use anyhow::{Context, Result};
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_endpoints, create_proxy_and_stream, Proxy};
    use fidl_fuchsia_component_sandbox as fsandbox;
    use fidl_fuchsia_unknown as funknown;
    use fuchsia_zircon::{self as zx, HandleBased};
    use futures::try_join;

    /// Tests that the OneShotHandle implementation of the HandleCapability.GetHandle method
    /// returns the handle held by the OneShotHandle.
    #[fuchsia::test]
    async fn one_shot_serve_get_handle() -> Result<()> {
        // Create an Event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let one_shot = OneShotHandle::from(event.into_handle());

        let (handle_proxy, handle_stream) =
            create_proxy_and_stream::<fsandbox::HandleCapabilityMarker>()?;
        let server = one_shot.serve_handle_capability(handle_stream);

        let client = async move {
            let handle = handle_proxy.get_handle().await.unwrap().unwrap();

            // The handle should be for same Event that was in the OneShotHandle.
            let got_koid = handle.get_koid().unwrap();
            assert_eq!(got_koid, expected_koid);

            Ok(())
        };

        try_join!(client, server).unwrap();

        Ok(())
    }

    /// Tests that the OneShotHandle implementation of the HandleCapability.GetHandle method
    /// returns the Unavailable error if GetHandle is called twice.
    #[fuchsia::test]
    async fn one_shot_serve_get_handle_unavailable() -> Result<()> {
        let event = zx::Event::create();
        let one_shot = OneShotHandle::from(event.into_handle());

        let (handle_proxy, handle_stream) =
            create_proxy_and_stream::<fsandbox::HandleCapabilityMarker>()?;
        let server = one_shot.serve_handle_capability(handle_stream);

        let client = async move {
            let first_result = handle_proxy.get_handle().await.unwrap();
            assert!(first_result.is_ok());

            let second_result = handle_proxy.get_handle().await.unwrap();
            assert_eq!(Err(fsandbox::HandleCapabilityError::Unavailable), second_result);

            Ok(())
        };

        try_join!(client, server).unwrap();

        Ok(())
    }

    #[fuchsia::test]
    async fn one_shot_into_fidl() -> Result<()> {
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let one_shot = OneShotHandle::from(event.into_handle());

        // Convert the OneShotHandle to FIDL and back.
        let fidl_capability: fsandbox::Capability = one_shot.into();

        let any: Capability = fidl_capability.try_into().context("failed to convert from FIDL")?;
        let one_shot = assert_matches!(any, Capability::OneShotHandle(h) => h);

        // Get the handle.
        let handle = one_shot.get_handle().unwrap();

        // The handle should be for same Event that was in the original OneShotHandle.
        let got_koid = handle.get_koid().unwrap();
        assert_eq!(got_koid, expected_koid);

        Ok(())
    }

    /// Tests that a OneShotHandle can be cloned via `fuchsia.unknown/Cloneable.Clone2`
    #[fuchsia::test]
    async fn fidl_clone() -> Result<()> {
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let one_shot = OneShotHandle::from(event.into_handle());

        let client_end: ClientEnd<fsandbox::HandleCapabilityMarker> = one_shot.into();
        let handle_cap_proxy = client_end.into_proxy().unwrap();

        // Clone the HandleCapability with `Clone2`
        let (clone_client_end, clone_server_end) = create_endpoints::<funknown::CloneableMarker>();
        let _ = handle_cap_proxy.clone2(clone_server_end);
        let clone_client_end: ClientEnd<fsandbox::HandleCapabilityMarker> =
            clone_client_end.into_channel().into();
        let clone_proxy = clone_client_end.into_proxy().unwrap();

        // Get the handle from the clone.
        let handle = clone_proxy.get_handle().await.context("failed to call GetHandle")?.unwrap();

        // The handle should be for same Event that was in the original OneShotHandle.
        let got_koid = handle.get_koid().unwrap();
        assert_eq!(got_koid, expected_koid);

        // Convert the original FIDL HandleCapability back to a Rust object.
        let handle_cap_client_end = ClientEnd::<fsandbox::HandleCapabilityMarker>::new(
            handle_cap_proxy.into_channel().unwrap().into_zx_channel(),
        );
        let fidl_capability = fsandbox::Capability::Handle(handle_cap_client_end);
        let any: Capability = fidl_capability.try_into().unwrap();
        let one_shot = assert_matches!(any, Capability::OneShotHandle(h) => h);

        // The original OneShotHandle should now not have a handle because it was taken
        // out by the GetHandle call on the clone.
        assert_matches!(one_shot.get_handle(), Err(fsandbox::HandleCapabilityError::Unavailable));

        Ok(())
    }
}
