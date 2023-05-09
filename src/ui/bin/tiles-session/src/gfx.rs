// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::base::{run_tile_controller_request_stream, MessageInternal, TileId, TilesSession},
    anyhow::{anyhow, Context, Error},
    async_trait::async_trait,
    fidl::{
        endpoints::{create_endpoints, create_proxy, ControlHandle, RequestStream, ServerEnd},
        AsHandleRef, HandleBased,
    },
    fidl_fuchsia_session_scene as scene, fidl_fuchsia_ui_gfx as ui_gfx,
    fidl_fuchsia_ui_scenic as ui_scenic, fidl_fuchsia_ui_views as ui_views,
    fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol,
    fuchsia_scenic::{self as scenic, ViewRefPair, ViewTokenPair},
    fuchsia_zircon as zx,
    futures::{channel::mpsc::UnboundedSender, TryStreamExt},
    std::collections::HashMap,
    tracing::{error, warn},
};

struct GfxChildView {
    view_holder: scenic::ViewHolder,
}

#[allow(dead_code)]
pub struct GfxTilesSession {
    internal_sender: UnboundedSender<MessageInternal>,
    session: scenic::SessionPtr,
    session_event_stream: ui_scenic::SessionEventStream,
    session_listener: ServerEnd<ui_scenic::SessionListenerMarker>,
    view_focuser: ui_views::FocuserProxy,
    root_view: scenic::View,
    root_node: scenic::EntityNode,
    tiles: HashMap<TileId, GfxChildView>,
    next_tile_id: u64,
    // NOTE: display_width/height are set only once in the "constructor" so tiles-session cannot
    // adjust to changes in screen resolution.  Instead of getting the display resolution directly
    // from Scenic, we could instead listen for ViewProperty changes.  However, this would be
    // premature, since `scene_manager` also doesn't deal with resolution changes.
    display_width: f32,
    display_height: f32,
    // Used by throttled_present(); protected by session mutex.
    present_in_flight: bool,
    // Used by throttled_present(); protected by session mutex.
    present_requested: bool,
}

#[async_trait]
impl TilesSession for GfxTilesSession {
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
                // Generate ID for the tile we've been asked to create.
                let new_tile_id = TileId(self.next_tile_id);
                self.next_tile_id += 1;

                // We have either a view holder token OR a viewport creation token, but for
                // Gfx we can expect a viewholder token.
                let view_holder = match view_spec.view_holder_token {
                    Some(view_holder_token) => scenic::ViewHolder::new(
                        self.session.clone(),
                        view_holder_token,
                        Some(format!("tiles-session tile {new_tile_id}")),
                    ),
                    None => {
                        warn!(
                            "Client attempted to present Flatland component inside of Gfx session."
                        );
                        return Ok(());
                    }
                };

                if let Some(view_ref) = view_spec.view_ref {
                    let _ = self
                        .view_focuser
                        .set_auto_focus(ui_views::FocuserSetAutoFocusRequest {
                            view_ref: Some(view_ref),
                            ..Default::default()
                        })
                        .await?;
                } else {
                    warn!("tile-session child doesn't have ViewRef, so not setting focus.");
                }

                {
                    /// The depth of the bounds of any added views. This can be used to compute
                    /// where a view should be placed to render "in front of" another view.
                    /// TODO(fxbug.dev/24474): -1000 is hardcoded in a lot of other locations, so
                    /// don't change this unless you're sure it's safe.
                    const VIEW_BOUNDS_DEPTH: f32 = -1000.0;

                    let view_properties = ui_gfx::ViewProperties {
                        bounding_box: ui_gfx::BoundingBox {
                            min: ui_gfx::Vec3 { x: 0.0, y: 0.0, z: VIEW_BOUNDS_DEPTH },
                            max: ui_gfx::Vec3 {
                                x: self.display_width,
                                y: self.display_height,
                                z: 0.0,
                            },
                        },
                        downward_input: true,
                        focus_change: true,
                        inset_from_min: ui_gfx::Vec3 { x: 0.0, y: 0.0, z: 0.0 },
                        inset_from_max: ui_gfx::Vec3 { x: 0.0, y: 0.0, z: 0.0 },
                    };
                    view_holder.set_view_properties(view_properties);
                }

                self.root_node.remove_all_children();
                self.root_node.add_child(&view_holder);

                self.throttled_present().await?;

                // Track all of the child view's resources.
                self.tiles.insert(new_tile_id, GfxChildView { view_holder });

                // Alert the client that the view has been presented, then begin servicing ViewController requests.
                let view_controller_request_stream = view_controller_request_stream.unwrap();
                view_controller_request_stream.control_handle().send_on_presented()?;
                run_tile_controller_request_stream(
                    new_tile_id,
                    view_controller_request_stream,
                    self.internal_sender.clone(),
                );

                // Finally, acknowledge the PresentView request.
                if let Err(e) = responder.send(&mut Ok(())) {
                    error!("Failed to send response for GraphicalPresenter.PresentView(): {}", e);
                    return Err(anyhow!(e));
                }

                // Ignore Annotations for now.
                let _ = annotation_controller;

                Ok(())
            }
            MessageInternal::DismissClient { tile_id, control_handle } => {
                // Explicitly shutting down the handle indicates intentionality, instead of
                // (for example) because this component crashed and the handle was auto-closed.
                control_handle.shutdown_with_epitaph(zx::Status::OK);
                match self.tiles.remove(&tile_id) {
                    Some(tile) => self.release_tile_resources(tile).await?,
                    None => error!("Tile not found after client requested dismiss: {tile_id}"),
                }
                Ok(())
            }
            MessageInternal::ClientDied { tile_id } => {
                match self.tiles.remove(&tile_id) {
                    Some(tile) => {
                        self.release_tile_resources(tile).await?;
                    }
                    None => error!("Tile not found after client died: {tile_id}"),
                }
                Ok(())
            }
            MessageInternal::ReceivedClientViewRef { tile_id, view_ref } => {
                let result = self.view_focuser.request_focus(view_ref);
                fasync::Task::local(async move {
                    match result.await {
                        Ok(Ok(())) => {
                            error!("TILES Success when requesting focus on child {tile_id}")
                        }
                        Ok(Err(e)) => {
                            error!(" TILES Error while requesting focus on child {tile_id}: {e:?}")
                        }
                        Err(e) => error!(
                            "TILES FIDL error while requesting focus on child {tile_id}: {e:?}"
                        ),
                    }
                })
                .detach();
                Ok(())
            }
        }
    }
}

impl GfxTilesSession {
    pub async fn new(
        scenic: &ui_scenic::ScenicProxy,
        internal_sender: UnboundedSender<MessageInternal>,
    ) -> Result<GfxTilesSession, Error> {
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
        let ViewTokenPair { view_token, view_holder_token } = ViewTokenPair::new()?;
        let ViewRefPair { control_ref, view_ref } = ViewRefPair::new()?;
        let fut = scene_manager.present_root_view_legacy(
            view_holder_token,
            ui_views::ViewRef {
                reference: view_ref.reference.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            },
        );
        let tiles_session = Self::create_gfx_tiles_session(
            internal_sender,
            scenic,
            view_token,
            control_ref,
            view_ref,
        )
        .await?;
        let _ = fut.await?;

        Ok(tiles_session)
    }

    // Helper called by `new()`.
    async fn create_gfx_tiles_session(
        internal_sender: UnboundedSender<MessageInternal>,
        scenic: &ui_scenic::ScenicProxy,
        view_token: ui_views::ViewToken,
        view_ref_control: ui_views::ViewRefControl,
        view_ref: ui_views::ViewRef,
    ) -> Result<GfxTilesSession, Error> {
        let display_info = scenic.get_display_info().await?;
        let width_in_pixels = display_info.width_in_px as f32;
        let height_in_pixels = display_info.height_in_px as f32;

        let (session_listener, session_listener_request) = create_endpoints();
        let (session_proxy, session_request) = create_proxy()?;
        let (view_focuser, view_focuser_request) = create_proxy::<ui_views::FocuserMarker>()?;
        scenic
            .create_session2(session_request, Some(session_listener), Some(view_focuser_request))
            .expect("failed to create scenic Session");

        session_proxy.set_debug_name("tiles-session")?;

        let session_event_stream = session_proxy.take_event_stream();
        let scenic_session = scenic::Session::new(session_proxy);

        // The caller must guarantee these are all valid.
        assert!(!view_token.value.as_handle_ref().is_invalid());
        assert!(!view_ref_control.reference.as_handle_ref().is_invalid());
        assert!(!view_ref.reference.as_handle_ref().is_invalid());

        let root_view = scenic::View::new3(
            scenic_session.clone(),
            view_token,
            view_ref_control,
            view_ref,
            Some(String::from("tiles-session root view (Gfx)")),
        );

        let root_node = scenic::EntityNode::new(scenic_session.clone());
        root_view.add_child(&root_node);

        let mut gfx_tiles_session = GfxTilesSession {
            internal_sender,
            session: scenic_session,
            session_event_stream,
            session_listener: session_listener_request,
            view_focuser,
            root_view,
            root_node,
            tiles: HashMap::new(),
            next_tile_id: 0,
            display_width: width_in_pixels,
            display_height: height_in_pixels,
            present_in_flight: false,
            present_requested: false,
        };

        gfx_tiles_session.throttled_present().await?;

        Ok(gfx_tiles_session)
    }

    // Ensure that only one present is in flight at any given time.  If a present is in flight when
    // another present is requested, wait for an `OnFramePresented` event before presenting again.
    // In this case, `throttled_present()` immediately returns `Ok()`.
    //
    // NOTE: when a present is already in flight, calls to `throttled_present()` are not 1-1 with
    // Scenic present calls.  In the hopes of preemptively reducing debugging confusion, we keep
    // track of the number of present calls that have been made by this invocation of
    // `throttled_present()`.  If it is > 1, then the failure is due to a present triggered by a
    // subsequent call to `throttled_present()`.
    async fn throttled_present(&mut self) -> Result<(), Error> {
        let mut present_count = 1;

        loop {
            let present_future = {
                let mut locked_session = self.session.lock();

                // If a present is already in flight, but hasn't been acknowledged by an `OnFramePresented`
                // event, remember that we would like another present as soon as the previous one is done.
                // Whoever receives `OnFramePresented` will initiate the next present.
                if self.present_in_flight {
                    self.present_requested = true;
                    return Ok(());
                }

                self.present_in_flight = true;
                self.present_requested = false;
                locked_session.present2(0, 0)
            };
            present_future.await.context(format!(
                "throttled_present() failed call to present2 (present_count: {present_count})"
            ))?;

            // This invocation of throttled_present() is the only one waiting for an `OnFramePresented`
            // event, because if there had already been a present in flight we would have returned
            // earlier, above.
            self.wait_for_on_frame_presented().await.context(format!(
                "throttled_present() failed to receive on_frame_presented (present_count: {present_count})"
            ))?;

            {
                // We're not actually using the locked session; we're using the mutex to protect
                // `present_in_flight` and `present_requested`.
                let _locked_session = self.session.lock();
                assert!(self.present_in_flight);
                self.present_in_flight = false;
                if !self.present_requested {
                    return Ok(());
                }
                self.present_requested = false;
            }

            // If we didn't already return above, it means that someone else requested a present.
            // It's our responsibility to return to the top of the loop and service it.
            present_count += 1;
        }
    }

    // Helper for `throttled_present()`.  We assume there will eventually be an `OnFramePresented`
    // event.  There should be, since this is called after a successful `present2` call.
    async fn wait_for_on_frame_presented(&mut self) -> Result<(), Error> {
        match self.session_event_stream.try_next().await {
            Ok(Some(ui_scenic::SessionEvent::OnFramePresented { frame_presented_info: _ })) => {
                return Ok(());
            }
            Ok(None) => {
                return Err(anyhow::format_err!("wait_for_on_frame_presented() failed to obtain next event from SessionListener: stream ended"));
            }
            Err(e) => {
                error!("wait_for_on_frame_presented() failed to obtain next event from SessionListener due to error: {}", e);
                return Err(anyhow!(e));
            }
        }
    }

    // `tile` will be dropped by this method.  However, we still need to detach it from the root,
    // otherwise Scenic will keep it alive even though we have dropped the client-side reference.
    async fn release_tile_resources(&mut self, tile: GfxChildView) -> Result<(), Error> {
        tile.view_holder.detach();
        return self.throttled_present().await;
    }
}
