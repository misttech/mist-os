// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_ui_test_conformance::{
        PuppetEmbedRemoteViewResponse, PuppetFactoryCreateResponse, PuppetFactoryRequest,
        PuppetFactoryRequestStream, PuppetRequest, PuppetRequestStream,
        PuppetSetEmbeddedViewPropertiesResponse, Result_,
    },
    futures::TryStreamExt,
    std::{cell::RefCell, rc::Rc},
    tracing::info,
};

mod presentation_loop;
mod view;

async fn run_puppet(request_stream: PuppetRequestStream, puppet_view: Rc<RefCell<view::View>>) {
    info!("Starting puppet instance");

    request_stream
        .try_for_each(|request| async {
            match request {
                PuppetRequest::EmbedRemoteView { payload, responder, .. } => {
                    let viewport_id = payload.id.expect("missing viewport id");
                    let properties = payload.properties.expect("missing embedded view properties");

                    let view_creation_token =
                        puppet_view.borrow_mut().embed_remote_view(viewport_id, properties).await;

                    responder
                        .send(PuppetEmbedRemoteViewResponse {
                            result: Some(Result_::Success),
                            view_creation_token: Some(view_creation_token),
                            ..Default::default()
                        })
                        .expect("failed to respond to EmbedRemoteView request");
                }
                PuppetRequest::SetEmbeddedViewProperties { payload, responder, .. } => {
                    let viewport_id = payload.id.expect("missing viewport id");
                    let properties = payload.properties.expect("missing embedded view properties");

                    puppet_view
                        .borrow_mut()
                        .set_embedded_view_properties(viewport_id, properties)
                        .await;

                    responder
                        .send(&PuppetSetEmbeddedViewPropertiesResponse {
                            result: Some(Result_::Success),
                            ..Default::default()
                        })
                        .expect("failed to respond to SetEmbeddedViewProperties request");
                }
                _ => {
                    panic!("unsupported operation");
                }
            }

            Ok(())
        })
        .await
        .expect("failed to serve puppet stream");

    info!("reached end of puppet request stream; closing connection");
}

pub async fn run_puppet_factory(request_stream: PuppetFactoryRequestStream) {
    info!("handling client connection to puppet factory service");

    request_stream
        .try_for_each_concurrent(None, |request| async {
            match request {
                PuppetFactoryRequest::Create { payload, responder, .. } => {
                    info!("create puppet");
                    let view_token =
                        payload.view_token.expect("missing puppet viewport creation token");
                    let puppet_server = payload.server_end.expect("missing puppet server endpoint");
                    let touch_listener = match payload.touch_listener {
                        None => None,
                        Some(touch_listener) => {
                            Some(touch_listener.into_proxy().expect("failed to generate proxy"))
                        }
                    };
                    let mouse_listener = match payload.mouse_listener {
                        None => None,
                        Some(mouse_listener) => {
                            Some(mouse_listener.into_proxy().expect("failed to generate proxy"))
                        }
                    };
                    let keyboard_listener = match payload.keyboard_listener {
                        None => None,
                        Some(keyboard_listener) => {
                            Some(keyboard_listener.into_proxy().expect("failed to generate proxy"))
                        }
                    };

                    let view = view::View::new(
                        view_token,
                        touch_listener,
                        mouse_listener,
                        keyboard_listener,
                    )
                    .await;

                    responder
                        .send(&PuppetFactoryCreateResponse {
                            result: Some(Result_::Success),
                            ..Default::default()
                        })
                        .expect("failed to respond to PuppetFactoryRequest::Create");

                    run_puppet(
                        puppet_server.into_stream().expect("failed to bind puppet server endpoint"),
                        view,
                    )
                    .await;
                }
            }

            Ok(())
        })
        .await
        .expect("failed to serve puppet factory stream");

    info!("reached end of puppet factory request stream; closing connection");
}
