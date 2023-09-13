// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::base::{run_tile_controller_request_stream, MessageInternal, TileId, TilesSession},
    anyhow::{Context, Error},
    async_trait::async_trait,
    fidl::endpoints::{create_proxy, ControlHandle, Proxy, RequestStream},
    fidl_fuchsia_session_scene as scene, fidl_fuchsia_ui_composition as ui_comp,
    fidl_fuchsia_ui_views as ui_views, fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol,
    fuchsia_scenic::{
        flatland::IdGenerator as FlatlandIdGenerator, flatland::ViewCreationTokenPair, ViewRefPair,
    },
    fuchsia_zircon as zx,
    futures::channel::mpsc::UnboundedSender,
    std::collections::HashMap,
    tracing::{error, info, warn},
};

struct FlatlandChildView {
    viewport_transform_id: ui_comp::TransformId,
    viewport_content_id: ui_comp::ContentId,
}

pub struct FlatlandTilesSession {
    internal_sender: UnboundedSender<MessageInternal>,
    flatland: ui_comp::FlatlandProxy,
    id_generator: FlatlandIdGenerator,
    view_focuser: ui_views::FocuserProxy,
    root_transform_id: ui_comp::TransformId,
    layout_info: ui_comp::LayoutInfo,
    tiles: HashMap<TileId, FlatlandChildView>,
    next_tile_id: u64,
}

impl Drop for FlatlandTilesSession {
    fn drop(&mut self) {
        let flatland = &self.flatland;
        let root_transform = &mut self.root_transform_id;
        let tiles = &mut self.tiles;
        tiles.retain(|key, tile| {
            if let Err(e) = Self::release_tile_resources(flatland, tile) {
                error!("Error releasing resources for tile {key}: {e}");
            }
            false
        });
        if let Err(e) = flatland.release_transform(root_transform) {
            error!("Error releasing root transform: {e}");
        }
        if let Err(e) = flatland.release_view() {
            error!("Error releasing view: {e}");
        }
    }
}

#[async_trait]
impl TilesSession for FlatlandTilesSession {
    async fn handle_message(&mut self, message: MessageInternal) -> Result<(), Error> {
        match message {
            // The ElementManager has asked us (via GraphicalPresenter::PresentView()) to display
            // the view provided by a newly-launched element.
            MessageInternal::GraphicalPresenterPresentView {
                view_spec,
                annotation_controller,
                view_controller_request_stream,
                responder,
            } => {
                // We have either a view holder token OR a viewport_creation_token, but for
                // Flatland we can expect a viewport creation token.
                let viewport_creation_token = match view_spec.viewport_creation_token {
                    Some(token) => token,
                    None => {
                        warn!("Client attempted to present Gfx component but only Flatland is supported.");
                        return Ok(());
                    }
                };

                // Create a Viewport that houses the view we are creating.
                let (tile_watcher, tile_watcher_request) =
                    create_proxy::<ui_comp::ChildViewWatcherMarker>()?;
                let viewport_content_id = self.id_generator.next_content_id();
                let viewport_properties = ui_comp::ViewportProperties {
                    logical_size: Some(self.layout_info.logical_size.unwrap()),
                    ..Default::default()
                };
                self.flatland
                    .create_viewport(
                        &viewport_content_id,
                        viewport_creation_token,
                        &viewport_properties,
                        tile_watcher_request,
                    )
                    .context("GraphicalPresenterPresentView create_viewport")?;

                // Attach the Viewport to the scene graph.
                let viewport_transform_id = self.id_generator.next_transform_id();
                self.flatland
                    .create_transform(&viewport_transform_id)
                    .context("GraphicalPresenterPresentView create_transform")?;
                self.flatland
                    .set_content(&viewport_transform_id, &viewport_content_id)
                    .context("GraphicalPresenterPresentView create_transform")?;
                self.flatland
                    .add_child(&self.root_transform_id, &viewport_transform_id)
                    .context("GraphicalPresenterPresentView add_child")?;

                // Flush the changes.
                self.flatland
                    .present(ui_comp::PresentArgs {
                        requested_presentation_time: Some(0),
                        ..Default::default()
                    })
                    .context("GraphicalPresenterPresentView present")?;

                // Track all of the child view's resources.
                let new_tile_id = TileId(self.next_tile_id);
                self.next_tile_id += 1;
                self.tiles.insert(
                    new_tile_id,
                    FlatlandChildView { viewport_transform_id, viewport_content_id },
                );

                // Alert the client that the view has been presented, then begin servicing ViewController requests.
                let view_controller_request_stream = view_controller_request_stream.unwrap();
                view_controller_request_stream
                    .control_handle()
                    .send_on_presented()
                    .context("GraphicalPresenterPresentView send_on_presented")?;
                run_tile_controller_request_stream(
                    new_tile_id,
                    view_controller_request_stream,
                    self.internal_sender.clone(),
                );

                // Begin servicing ChildViewWatcher requests.
                Self::watch_tile(new_tile_id, tile_watcher, self.internal_sender.clone());

                // Ignore Annotations for now.
                let _ = annotation_controller;

                // Finally, acknowledge the PresentView request.
                if let Err(e) = responder.send(Ok(())) {
                    error!("Failed to send response for GraphicalPresenter.PresentView(): {}", e);
                }

                Ok(())
            }
            MessageInternal::DismissClient { tile_id, control_handle } => {
                // Explicitly shutting down the handle indicates intentionality, instead of
                // (for example) because this component crashed and the handle was auto-closed.
                control_handle.shutdown_with_epitaph(zx::Status::OK);
                match &mut self.tiles.remove(&tile_id) {
                    Some(tile) => Self::release_tile_resources(&self.flatland, tile)
                        .context("DismissClient release_tile_resources")?,
                    None => error!("Tile not found after client requested dismiss: {tile_id}"),
                }

                Ok(())
            }
            MessageInternal::ClientDied { tile_id } => {
                match &mut self.tiles.remove(&tile_id) {
                    Some(tile) => Self::release_tile_resources(&self.flatland, tile)
                        .context("ClientDied release_tile_resources")?,
                    None => error!("Tile not found after client died: {tile_id}"),
                }

                Ok(())
            }
            MessageInternal::ReceivedClientViewRef { tile_id, view_ref, .. } => {
                let result = self.view_focuser.request_focus(view_ref);
                fasync::Task::local(async move {
                    match result.await {
                        Ok(Ok(())) => {
                            info!("Successfully requested focus on child {tile_id}")
                        }
                        Ok(Err(e)) => {
                            error!("Error while requesting focus on child {tile_id}: {e:?}")
                        }
                        Err(e) => {
                            error!("FIDL error while requesting focus on child {tile_id}: {e:?}")
                        }
                    }
                })
                .detach();

                Ok(())
            }
        }
    }
}

impl FlatlandTilesSession {
    pub async fn new(
        internal_sender: UnboundedSender<MessageInternal>,
    ) -> Result<FlatlandTilesSession, Error> {
        // TODO(fxbug.dev/88656): do something like this to instantiate the library component that knows
        // how to generate a Flatland scene to lay views out on a tiled grid.  It will be used in the
        // event loop below.
        // let tiles_helper = tile_helper::TilesHelper::new();

        // Set the root view and then wait for scene_manager to reply with a CreateView2 request.
        // Don't await the result yet, because the future will not resolve until we handle the
        // ViewProvider request below.
        let scene_manager = connect_to_protocol::<scene::ManagerMarker>()
            .expect("failed to connect to fuchsia.scene.Manager");

        // TODO(fxbug.dev/104411): see scene_manager.fidl.  If we awaited the future immediately we
        // would deadlock.  Conversely, if we just dropped the future, then scene_manager would barf
        // because it would try to reply to present_root_view() on a closed channel.  So we kick off
        // the async FIDL request (which is not idiomatic for Rust, where typically the "future
        // doesn't do anything" until awaited), and then call create_flatland_tiles_session() so
        // that present_root_view() eventually returns a result.
        let ViewCreationTokenPair { view_creation_token, viewport_creation_token } =
            ViewCreationTokenPair::new()?;
        let fut = scene_manager.present_root_view(viewport_creation_token);
        let tiles_session =
            Self::create_flatland_tiles_session(view_creation_token, internal_sender).await?;
        let _ = fut.await?;
        Ok(tiles_session)
    }

    async fn create_flatland_tiles_session(
        view_creation_token: ui_views::ViewCreationToken,
        internal_sender: UnboundedSender<MessageInternal>,
    ) -> Result<FlatlandTilesSession, Error> {
        let flatland = connect_to_protocol::<ui_comp::FlatlandMarker>()
            .expect("failed to connect to fuchsia.ui.flatland.Flatland");
        let mut id_generator = FlatlandIdGenerator::new();

        // Create the root transform for tiles.
        let root_transform_id = id_generator.next_transform_id();
        flatland.create_transform(&root_transform_id)?;
        flatland.set_root_transform(&root_transform_id)?;

        // Create the root view for tiles.
        let (parent_viewport_watcher, parent_viewport_watcher_request) =
            create_proxy::<ui_comp::ParentViewportWatcherMarker>()
                .expect("Failed to create ParentViewportWatcher channel");
        let (view_focuser, view_focuser_request) =
            fidl::endpoints::create_proxy::<ui_views::FocuserMarker>()
                .expect("Failed to create Focuser channel");
        let view_identity = ui_views::ViewIdentityOnCreation::from(ViewRefPair::new()?);
        let view_bound_protocols = ui_comp::ViewBoundProtocols {
            view_focuser: Some(view_focuser_request),
            ..Default::default()
        };
        flatland.create_view2(
            view_creation_token,
            view_identity,
            view_bound_protocols,
            parent_viewport_watcher_request,
        )?;

        // Present the root scene.
        flatland.present(ui_comp::PresentArgs {
            requested_presentation_time: Some(0),
            ..Default::default()
        })?;

        // Get initial layout deterministically before proceeding.
        // Begin servicing ParentViewportWatcher requests.
        let layout_info = parent_viewport_watcher.get_layout().await?;
        Self::watch_layout(parent_viewport_watcher, internal_sender.clone());

        Ok(FlatlandTilesSession {
            internal_sender,
            flatland: flatland,
            id_generator,
            view_focuser,
            root_transform_id,
            layout_info,
            tiles: HashMap::new(),
            next_tile_id: 0,
        })
    }

    fn release_tile_resources(
        flatland: &ui_comp::FlatlandProxy,
        tile: &mut FlatlandChildView,
    ) -> Result<(), Error> {
        let _ = flatland.release_viewport(&tile.viewport_content_id);
        flatland.release_transform(&tile.viewport_transform_id)?;
        Ok(())
    }

    fn watch_layout(
        proxy: ui_comp::ParentViewportWatcherProxy,
        _internal_sender: UnboundedSender<MessageInternal>,
    ) {
        // Listen for channel closure.
        // TODO(fxbug.dev/88656): Actually watch for and respond to layout changes.
        fasync::Task::local(async move {
            let _ = proxy.on_closed().await;
        })
        .detach();
    }

    fn watch_tile(
        tile_id: TileId,
        proxy: ui_comp::ChildViewWatcherProxy,
        internal_sender: UnboundedSender<MessageInternal>,
    ) {
        // Get view ref, then listen for channel closure.
        fasync::Task::local(async move {
            match proxy.get_view_ref().await {
                Ok(view_ref) => {
                    internal_sender
                        .unbounded_send(MessageInternal::ReceivedClientViewRef {
                            tile_id,
                            view_ref,
                        })
                        .expect("Failed to send MessageInternal::ReceivedClientViewRef");
                }
                Err(_) => {
                    internal_sender
                        .unbounded_send(MessageInternal::ClientDied { tile_id })
                        .expect("Failed to send MessageInternal::ClientDied");
                    return;
                }
            }

            let _ = proxy.on_closed().await;

            internal_sender
                .unbounded_send(MessageInternal::ClientDied { tile_id })
                .expect("Failed to send MessageInternal::ClientDied");
        })
        .detach();
    }
}
