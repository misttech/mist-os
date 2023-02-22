// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    async_trait::async_trait,
    fidl_fuchsia_element as element, fidl_fuchsia_ui_views as ui_views, fuchsia_async as fasync,
    futures::{channel::mpsc::UnboundedSender, StreamExt},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileId(pub u64);

impl std::fmt::Display for TileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "id={}", self.0)
    }
}

pub enum MessageInternal {
    GraphicalPresenterPresentView {
        view_spec: element::ViewSpec,
        annotation_controller: Option<element::AnnotationControllerProxy>,
        view_controller_request_stream: Option<element::ViewControllerRequestStream>,
        responder: element::GraphicalPresenterPresentViewResponder,
    },
    DismissClient {
        tile_id: TileId,
        control_handle: element::ViewControllerControlHandle,
    },
    ClientDied {
        tile_id: TileId,
    },
    ReceivedClientViewRef {
        tile_id: TileId,
        view_ref: ui_views::ViewRef,
    },
}

#[async_trait]
pub trait TilesSession {
    async fn handle_message(&mut self, message: MessageInternal) -> Result<(), Error>;
}

// Serve the fuchsia.element.ViewController protocol.  This merely redispatches the requests onto
// onto the `MessageInternal` handler; these messages are handled by the concrete implementation of
// the `TilesSession` trait.
pub fn run_tile_controller_request_stream(
    tile_id: TileId,
    mut request_stream: fidl_fuchsia_element::ViewControllerRequestStream,
    internal_sender: UnboundedSender<MessageInternal>,
) {
    fasync::Task::local(async move {
        if let Some(Ok(fidl_fuchsia_element::ViewControllerRequest::Dismiss { control_handle })) =
            request_stream.next().await
        {
            {
                internal_sender
                    .unbounded_send(MessageInternal::DismissClient { tile_id, control_handle })
                    .expect("Failed to send MessageInternal::DismissClient");
            }
        }
    })
    .detach();
}
