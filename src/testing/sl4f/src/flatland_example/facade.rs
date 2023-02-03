// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl::endpoints::{create_endpoints, ClientEnd, Proxy};
use fidl_fuchsia_developer_tiles::{
    ControllerMarker as TilesControllerMarker, ControllerProxy as TilesControllerProxy,
};
use fidl_fuchsia_ui_app::ViewProviderMarker;
use fuchsia_component::client::{connect_channel_to_protocol_at_path, connect_to_protocol};
use serde_json::{json, Value};
use std::cell::RefCell;
use tracing::info;

pub struct FlatlandExampleFacade {
    state: RefCell<Option<FacadeState>>,
}

impl std::fmt::Debug for FlatlandExampleFacade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlatlandExampleFacade").finish()
    }
}

impl FlatlandExampleFacade {
    pub fn new() -> FlatlandExampleFacade {
        FlatlandExampleFacade { state: RefCell::new(None) }
    }

    pub async fn start(&self) -> Result<Value, Error> {
        info!("Starting the flatland example app.");
        if self.state.borrow().is_none() {
            let new_state = FacadeState::start().await.context("starting example app")?;
            *self.state.borrow_mut() = Some(new_state);
        }
        Ok(json!({}))
    }

    pub async fn stop(&self) -> Result<Value, Error> {
        if let Some(old_state) = self.state.borrow_mut().take() {
            old_state.stop().await.context("stopping example app")?;
        }
        Ok(json!({}))
    }
}

// TODO(https://fxbug.dev/107905) switch to session APIs
struct FacadeState {
    tiles: TilesControllerProxy,
    tile_key: u32,
}

impl FacadeState {
    async fn start() -> Result<Self, Error> {
        info!("Starting Flatland example.");
        let view_provider =
            connect_to_protocol::<ViewProviderMarker>().context("connecting to ViewProvider")?;
        let view_provider = ClientEnd::from(
            view_provider.into_channel().expect("no clones, just created").into_zx_channel(),
        );

        info!("Adding Flatland example to tiles");
        let (tiles_client, tiles_server) = create_endpoints::<TilesControllerMarker>().unwrap();
        connect_channel_to_protocol_at_path(
            tiles_server.into_channel(),
            "/svc/fuchsia.developer.tiles.Controller.flatland",
        )
        .context("connecting to tiles.Controller")?;
        let tiles = tiles_client.into_proxy().unwrap();

        let url = "fuchsia-pkg://fuchsia.com/flatland-examples#meta/flatland-view-provider.cm";
        let tile_key = tiles
            .add_tile_from_view_provider(url, view_provider)
            .await
            .context("creating new tile for example")?;

        Ok(Self { tiles, tile_key })
    }

    async fn stop(self) -> Result<(), Error> {
        info!("Removing Flatland example from tiles");
        self.tiles.remove_tile(self.tile_key).context("removing flatland example tile")?;
        Ok(())
    }
}
