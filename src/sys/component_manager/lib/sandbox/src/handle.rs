// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCast, Capability, CloneError, ConversionError, Open},
    anyhow::{anyhow, Context},
    fidl::endpoints::ClientEnd,
    fidl_fuchsia_io as fio, fidl_fuchsia_unknown as funknown,
    fuchsia_zircon::{self as zx, AsHandleRef, HandleBased},
    futures::future::BoxFuture,
};

/// A capability that represents a Zircon handle.
#[derive(Capability, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Handle(zx::Handle);

impl zx::HandleBased for Handle {}

impl zx::AsHandleRef for Handle {
    fn as_handle_ref(&self) -> zx::HandleRef<'_> {
        self.0.as_handle_ref()
    }
}

impl Into<zx::Handle> for Handle {
    fn into(self) -> zx::Handle {
        self.0
    }
}

impl From<zx::Handle> for Handle {
    fn from(handle: zx::Handle) -> Self {
        Handle(handle)
    }
}

impl Handle {
    /// Attempts to clone by calling `fuchsia.unknown.Cloneable/Clone2` on the handle.
    ///
    /// The handle must be a channel that speaks the `Cloneable` protocol.
    fn try_fidl_clone(&self) -> Result<Self, CloneError> {
        // Try to clone. Only works for channels.
        let basic_info = self
            .0
            .basic_info()
            .map_err(|status| anyhow!("failed to get handle info: {}", status))?;
        if basic_info.object_type != zx::ObjectType::CHANNEL {
            return Err(CloneError::NotSupported);
        }

        let (client_end, server_end) = zx::Channel::create();
        let raw_handle = self.0.as_handle_ref().raw_handle();
        // SAFETY: the channel is forgotten at the end of scope so it is not double closed.
        unsafe {
            let borrowed: zx::Channel = zx::Handle::from_raw(raw_handle).into();
            let cloneable = funknown::CloneableSynchronousProxy::new(borrowed);
            // TODO(b/306037927): Calling Clone2 on a channel that doesn't speak Cloneable may
            // inadvertently close the channel.
            let result = cloneable.clone2(server_end.into());
            std::mem::forget(cloneable.into_channel());
            result.context("failed to call Clone2")?;
        }
        Ok(Self(client_end.into_handle()))
    }
}

impl Capability for Handle {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        (self.into(), None)
    }

    fn try_clone(&self) -> Result<Self, CloneError> {
        // Try to clone with `fuchsia.unknown.Cloneable`.
        if let Ok(clone) = self.try_fidl_clone() {
            return Ok(clone);
        }

        // Try to duplicate.
        if let Ok(dup) = self.0.duplicate_handle(zx::Rights::SAME_RIGHTS) {
            return Ok(Self(dup));
        }

        Err(CloneError::NotSupported)
    }

    fn try_into_capability(
        self,
        type_id: std::any::TypeId,
    ) -> Result<Box<dyn std::any::Any>, ConversionError> {
        if type_id == std::any::TypeId::of::<Self>() {
            return Ok(Box::new(self).into_any());
        } else if type_id == std::any::TypeId::of::<Open>() {
            let open: Open = self.try_into()?;
            return Ok(Box::new(open));
        }
        Err(ConversionError::NotSupported)
    }
}

impl TryInto<Open> for Handle {
    type Error = ConversionError;

    /// Attempts to convert into an Open that calls `fuchsia.io.Openable/Open` on this handle.
    ///
    /// The handle must be a channel that speaks the `Openable` protocol.
    fn try_into(self: Self) -> Result<Open, Self::Error> {
        let basic_info = self
            .0
            .basic_info()
            .map_err(|status| anyhow!("failed to get handle info: {}", status))?;
        if basic_info.object_type != zx::ObjectType::CHANNEL {
            return Err(ConversionError::NotSupported);
        }

        let openable = ClientEnd::<fio::OpenableMarker>::from(self.0)
            .into_proxy()
            .context("failed to convert to proxy")?;

        Ok(Open::new(
            move |_scope: vfs::execution_scope::ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: vfs::path::Path,
                  server_end: zx::Channel| {
                // TODO(b/306037927): Calling Open on a channel that doesn't speak Openable may
                // inadvertently close the channel.
                let _ = openable.open(
                    flags,
                    fio::ModeType::empty(),
                    relative_path.as_str(),
                    server_end.into(),
                );
            },
            // TODO(b/298112397): Determine a more accurate dirent type.
            fio::DirentType::Unknown,
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::{Capability, Handle, Open};
    use anyhow::Error;
    use fidl::endpoints::{create_endpoints, spawn_stream_handler, Proxy};
    use fidl_fuchsia_io as fio;
    use fidl_fuchsia_unknown as funknown;
    use fuchsia_zircon::{self as zx, HandleBased};
    use futures::{channel::mpsc, StreamExt};

    /// Tests that a handle to a channel that speaks `Cloneable` can be cloned.
    #[fuchsia::test]
    async fn try_clone_cloneable() -> Result<(), Error> {
        let (request_tx, mut request_rx) = mpsc::unbounded();

        let cloneable_proxy: funknown::CloneableProxy = spawn_stream_handler(move |request| {
            let request_tx = request_tx.clone();
            async move {
                match request {
                    funknown::CloneableRequest::Clone2 { request, control_handle: _ } => {
                        request_tx.unbounded_send(request).unwrap();
                    }
                }
            }
        })?;

        let zx_handle: zx::Handle =
            cloneable_proxy.into_channel().unwrap().into_zx_channel().into();
        let handle: Handle = zx_handle.into();

        let clone = handle.try_clone()?;
        assert!(!clone.is_invalid_handle());

        // Cloning should send a channel request to Clone2.
        let request = request_rx.next().await;
        assert!(request.is_some());

        Ok(())
    }

    /// Tests that a handle to a channel that speaks `Openable` can be converted to Open.
    #[fuchsia::test]
    async fn try_into_openable() -> Result<(), Error> {
        let (object_tx, mut object_rx) = mpsc::unbounded();

        let openable_proxy: fio::OpenableProxy = spawn_stream_handler(move |request| {
            let object_tx = object_tx.clone();
            async move {
                match request {
                    fio::OpenableRequest::Open { flags, mode, path, object, control_handle: _ } => {
                        assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                        assert_eq!(mode, fio::ModeType::empty());
                        assert_eq!(&path, "");
                        object_tx.unbounded_send(object).unwrap();
                    }
                }
            }
        })?;

        let zx_handle: zx::Handle = openable_proxy.into_channel().unwrap().into_zx_channel().into();
        let handle: Handle = zx_handle.into();

        let open: Open = handle.try_into()?;

        // Opening should send a server end to Open.
        let scope = vfs::execution_scope::ExecutionScope::new();
        let (_dir_client_end, dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        open.open(
            scope,
            fio::OpenFlags::DIRECTORY,
            ".".to_string(),
            dir_server_end.into_channel().into(),
        );

        let server_end = object_rx.next().await;
        assert!(server_end.is_some());

        Ok(())
    }
}
