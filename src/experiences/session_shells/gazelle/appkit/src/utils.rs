// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::AsHandleRef;
use fidl_fuchsia_element as felement;
use fidl_fuchsia_sysmem as sysmem;
use fidl_fuchsia_ui_composition as ui_comp;
use fidl_fuchsia_ui_focus as ui_focus;
use fidl_fuchsia_ui_input3 as ui_input3;
use fidl_fuchsia_ui_shortcut2 as ui_shortcut2;
use fidl_fuchsia_ui_views as ui_views;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use futures::TryFutureExt;

/// Defines a trait to implement connecting to all services dependent by the app.
pub trait ProtocolConnector: Send + Sync {
    fn connect_to_flatland(&self) -> Result<ui_comp::FlatlandProxy, Error>;
    fn connect_to_graphical_presenter(&self) -> Result<felement::GraphicalPresenterProxy, Error>;
    fn connect_to_shortcuts_registry(&self) -> Result<ui_shortcut2::RegistryProxy, Error>;
    fn connect_to_keyboard(&self) -> Result<ui_input3::KeyboardProxy, Error>;
    fn connect_to_focus_chain_listener(
        &self,
    ) -> Result<ui_focus::FocusChainListenerRegistryProxy, Error>;
    fn connect_to_sysmem_allocator(&self) -> Result<sysmem::AllocatorProxy, Error>;
    fn connect_to_flatland_allocator(&self) -> Result<ui_comp::AllocatorProxy, Error>;

    fn box_clone(&self) -> Box<dyn ProtocolConnector>;
}

/// Provides connecting to services available in the current execution environment.
#[derive(Clone)]
pub struct ProductionProtocolConnector();

impl ProtocolConnector for ProductionProtocolConnector {
    fn connect_to_flatland(&self) -> Result<ui_comp::FlatlandProxy, Error> {
        connect_to_protocol::<ui_comp::FlatlandMarker>()
    }

    fn connect_to_graphical_presenter(&self) -> Result<felement::GraphicalPresenterProxy, Error> {
        connect_to_protocol::<felement::GraphicalPresenterMarker>()
    }

    fn connect_to_shortcuts_registry(&self) -> Result<ui_shortcut2::RegistryProxy, Error> {
        connect_to_protocol::<ui_shortcut2::RegistryMarker>()
    }

    fn connect_to_keyboard(&self) -> Result<ui_input3::KeyboardProxy, Error> {
        connect_to_protocol::<ui_input3::KeyboardMarker>()
    }

    fn connect_to_focus_chain_listener(
        &self,
    ) -> Result<ui_focus::FocusChainListenerRegistryProxy, Error> {
        connect_to_protocol::<ui_focus::FocusChainListenerRegistryMarker>()
    }

    fn connect_to_sysmem_allocator(&self) -> Result<sysmem::AllocatorProxy, Error> {
        connect_to_protocol::<sysmem::AllocatorMarker>()
    }

    fn connect_to_flatland_allocator(&self) -> Result<ui_comp::AllocatorProxy, Error> {
        connect_to_protocol::<ui_comp::AllocatorMarker>()
    }

    fn box_clone(&self) -> Box<dyn ProtocolConnector> {
        Box::new(ProductionProtocolConnector())
    }
}

/// Returns true if the koid of the two view_ref parameters match.
pub fn view_ref_is_same(view_ref1: &ui_views::ViewRef, view_ref2: &ui_views::ViewRef) -> bool {
    view_ref1.reference.as_handle_ref().get_koid() == view_ref2.reference.as_handle_ref().get_koid()
}

/// Resolves the future's error value in a [`fuchsia_async`] spawned task.
///
/// The provided closure `e` will only be called if this future is resolved
/// to an [`Err`]. If it resolves to an [`Ok`], panics, or is dropped, then
/// the provided closure will never be invoked.
///
/// Note that this method consumes the future it is called.
/// See: https://docs.rs/futures-util/latest/futures_util/future/trait.TryFutureExt.html#method.map_err
pub fn spawn_async_on_err<Fut, E>(fut: Fut, e: E)
where
    Fut: TryFutureExt + Send + 'static,
    E: FnOnce(Fut::Error) + Send + 'static,
{
    fasync::Task::spawn(async move {
        let _ = fut.map_err(e).await;
    })
    .detach();
}

/// Resolves the future's success value in a [`fuchsia_async`] spawned task.
///
/// The provided closure `f` will only be called if this future is resolved
/// to an [`Ok`]. If it resolves to an [`Err`], panics, or is dropped, then
/// the provided closure will never be invoked.
///
/// Note that this method consumes the future it is called.
/// See: https://docs.rs/futures-util/latest/futures_util/future/trait.TryFutureExt.html#method.map_ok
pub fn spawn_async_on_ok<Fut, F>(fut: Fut, f: F)
where
    Fut: TryFutureExt + Send + 'static,
    F: FnOnce(Fut::Ok) + Send + 'static,
{
    fasync::Task::spawn(async move {
        let _ = fut.map_ok(f).await;
    })
    .detach();
}

/// Resolves the future in a [`fuchsia_async`] spawned task.
///
/// The provided closure `f` will only be called if this future is resolved
/// to an [`Ok`]. If it resolves to an [`Err`], panics, or is dropped, then
/// the provided closure will never be invoked.
///
/// The provided closure `e` will only be called if this future is resolved
/// to an [`Err`]. If it resolves to an [`Ok`], panics, or is dropped, then
/// the provided closure will never be invoked.
///
/// Note that this method consumes the future it is called.
/// See: https://docs.rs/futures-util/latest/futures_util/future/trait.TryFutureExt.html#method.map_ok_or_else
pub fn spawn_async_on_ok_or_else<Fut, F, E>(fut: Fut, f: F, e: E)
where
    Fut: TryFutureExt + Send + 'static,
    F: FnOnce(Fut::Ok) + Send + 'static,
    E: FnOnce(Fut::Error) + Send + 'static,
{
    fasync::Task::spawn(async move {
        let _ = fut.map_ok_or_else(e, f).await;
    })
    .detach();
}

/// Converts a 32-bit sRGB color value encoded as 0xRRGGBBAA into [ui_comp::ColorRgba].
pub fn srgb_to_linear(color: u32) -> ui_comp::ColorRgba {
    let to_linear = |x: u8| {
        let x = x as f32 / 255.0;
        if x >= 0.04045 {
            ((x + 0.055) / 1.055).powf(2.4)
        } else {
            x / 12.92
        }
    };
    let bytes: [u8; 4] = unsafe { std::mem::transmute(color.to_be()) };
    ui_comp::ColorRgba {
        red: to_linear(bytes[0]),
        green: to_linear(bytes[1]),
        blue: to_linear(bytes[2]),
        alpha: bytes[3] as f32 / 255.0,
    }
}

#[cfg(test)]
mod tests {
    use fidl_fuchsia_ui_composition::ColorRgba;

    use crate::srgb_to_linear;

    #[test]
    fn convert_color() {
        let x = srgb_to_linear(0x7f7f7fff);
        assert_eq!(
            x,
            ColorRgba { red: 0.21223073, green: 0.21223073, blue: 0.21223073, alpha: 1.0 }
        );
    }
}
