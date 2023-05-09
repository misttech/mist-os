// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    async_trait::async_trait,
    fidl_fuchsia_ui_app as ui_app, fidl_fuchsia_ui_views as ui_views,
    fidl_fuchsia_ui_views::ViewRef,
    scene_management::{DisplayMetrics, InjectorViewportSubscriber, SceneManager, ViewportToken},
    std::cell::Cell,
};

pub struct MockSceneManager {
    was_present_root_view_called: Cell<bool>,
    was_set_root_view_called: Cell<bool>,
    set_root_view_viewport_token: Cell<Option<ViewportToken>>,
    set_root_view_view_ref: Cell<Option<ViewRef>>,
}

impl MockSceneManager {
    pub fn new() -> Self {
        MockSceneManager {
            was_present_root_view_called: Cell::new(false),
            was_set_root_view_called: Cell::new(false),
            set_root_view_viewport_token: Cell::new(None),
            set_root_view_view_ref: Cell::new(None),
        }
    }

    pub fn assert_present_root_view_called(&self) {
        assert!(self.was_present_root_view_called.get() == true);
        self.was_present_root_view_called.set(false);
    }

    pub fn get_set_root_view_called_args(&self) -> (ViewportToken, Option<ui_views::ViewRef>) {
        assert!(self.was_set_root_view_called.get() == true);
        (
            self.set_root_view_viewport_token
                .take()
                .expect("Expected to have a viewport_token from a previous call to set_root_view."),
            self.set_root_view_view_ref.take(),
        )
    }
}

#[async_trait]
#[allow(unused_variables)]
impl SceneManager for MockSceneManager {
    fn present_root_view(&self) {
        self.was_present_root_view_called.set(true);
    }

    async fn set_root_view(
        &mut self,
        viewport_token: ViewportToken,
        view_ref: Option<ui_views::ViewRef>,
    ) -> Result<(), Error> {
        self.was_set_root_view_called.set(true);
        self.set_root_view_viewport_token.set(Some(viewport_token));
        self.set_root_view_view_ref.set(view_ref);
        Ok(())
    }

    // Leave everything else unimplemented.

    async fn set_root_view_deprecated(
        &mut self,
        view_provider: ui_app::ViewProviderProxy,
    ) -> Result<ui_views::ViewRef, Error> {
        unimplemented!()
    }

    fn request_focus(
        &self,
        view_ref: ui_views::ViewRef,
    ) -> fidl::client::QueryResponseFut<ui_views::FocuserRequestFocusResult> {
        unimplemented!()
    }

    async fn insert_a11y_view(
        &mut self,
        a11y_view_holder_token: ui_views::ViewHolderToken,
    ) -> Result<ui_views::ViewHolderToken, Error> {
        unimplemented!()
    }

    fn insert_a11y_view2(
        &mut self,
        a11y_viewport_creation_token: ui_views::ViewportCreationToken,
    ) -> Result<ui_views::ViewportCreationToken, Error> {
        unimplemented!()
    }

    async fn set_camera_clip_space_transform(&mut self, x: f32, y: f32, scale: f32) {
        unimplemented!()
    }

    async fn reset_camera_clip_space_transform(&mut self) {
        unimplemented!()
    }

    fn set_cursor_position(&mut self, position_physical_px: input_pipeline::Position) {
        unimplemented!()
    }

    fn set_cursor_visibility(&mut self, visible: bool) {
        unimplemented!()
    }

    fn get_pointerinjection_view_refs(&self) -> (ui_views::ViewRef, ui_views::ViewRef) {
        unimplemented!()
    }

    fn get_pointerinjection_display_size(&self) -> input_pipeline::Size {
        unimplemented!()
    }

    fn get_pointerinjector_viewport_watcher_subscription(&self) -> InjectorViewportSubscriber {
        unimplemented!()
    }

    fn get_display_metrics(&self) -> &DisplayMetrics {
        unimplemented!()
    }
}
