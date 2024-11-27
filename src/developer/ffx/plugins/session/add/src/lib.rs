// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use async_trait::async_trait;
use ffx_session_add_args::SessionAddCommand;
use fho::{moniker, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_element::{
    self as felement, Annotation, AnnotationKey, AnnotationValue, ControllerMarker, ManagerProxy,
    Spec,
};
use futures::channel::oneshot;
use futures::FutureExt;
use signal_hook::consts::signal::*;
use signal_hook::iterator::Signals;
use std::future::Future;

#[derive(FfxTool)]
pub struct AddTool {
    #[command]
    cmd: SessionAddCommand,
    #[with(moniker("/core/session-manager"))]
    manager_proxy: ManagerProxy,
}

fho::embedded_plugin!(AddTool);

#[async_trait(?Send)]
impl FfxMain for AddTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        add_impl(self.manager_proxy, self.cmd, spawn_ctrl_c_listener(), &mut writer).await?;
        Ok(())
    }
}

pub async fn add_impl<W: std::io::Write>(
    manager_proxy: ManagerProxy,
    cmd: SessionAddCommand,
    ctrl_c_signal: impl Future<Output = ()>,
    writer: &mut W,
) -> Result<()> {
    writeln!(writer, "Add {} to the current session", &cmd.url)?;
    let (controller_client, controller_server) = if cmd.interactive {
        let (client, server) = fidl::endpoints::create_endpoints::<ControllerMarker>();
        let client = client.into_proxy();
        (Some(client), Some(server))
    } else {
        (None, None)
    };

    let mut annotations = Vec::new();

    if cmd.persist {
        annotations.push(Annotation {
            key: AnnotationKey {
                namespace: felement::MANAGER_NAMESPACE.to_string(),
                value: felement::ANNOTATION_KEY_PERSIST_ELEMENT.to_string(),
            },
            value: AnnotationValue::Text(String::new()),
        });
    }

    if let Some(name) = cmd.name {
        annotations.push(Annotation {
            key: AnnotationKey {
                namespace: felement::MANAGER_NAMESPACE.to_string(),
                value: felement::ANNOTATION_KEY_NAME.to_string(),
            },
            value: AnnotationValue::Text(name),
        });
    }

    manager_proxy
        .propose_element(
            Spec {
                component_url: Some(cmd.url.to_string()),
                annotations: if annotations.is_empty() { None } else { Some(annotations) },
                ..Default::default()
            },
            controller_server,
        )
        .await?
        .map_err(|err| format_err!("{:?}", err))?;

    if controller_client.is_some() {
        // TODO(https://fxbug.dev/42058904) wait for either ctrl+c or the controller to close
        writeln!(writer, "Waiting for Ctrl+C before terminating element...")?;
        ctrl_c_signal.await;
    }

    Ok(())
}

/// Spawn a thread to listen for Ctrl+C, resolving the returned future when it's received.
fn spawn_ctrl_c_listener() -> impl Future<Output = ()> {
    let (sender, receiver) = oneshot::channel();
    std::thread::spawn(move || {
        let mut signals = Signals::new(&[SIGINT]).expect("must be able to create signal waiter");
        signals.forever().next().unwrap();
        sender.send(()).unwrap();
    });
    receiver.map(|_| ())
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use fidl_fuchsia_element::{self as felement, ManagerRequest};
    use futures::poll;

    #[fuchsia::test]
    async fn test_add_element() {
        const TEST_ELEMENT_URL: &str = "Test Element Url";

        let proxy = fho::testing::fake_proxy(|req| match req {
            ManagerRequest::ProposeElement { spec, responder, .. } => {
                assert_eq!(spec.component_url.unwrap(), TEST_ELEMENT_URL.to_string());
                let _ = responder.send(Ok(()));
            }
            ManagerRequest::RemoveElement { .. } => unreachable!(),
        });

        let add_cmd = SessionAddCommand {
            url: TEST_ELEMENT_URL.to_string(),
            interactive: false,
            persist: false,
            name: None,
        };
        let response =
            add_impl(proxy, add_cmd, spawn_ctrl_c_listener(), &mut std::io::stdout()).await;
        assert!(response.is_ok());
    }

    #[fuchsia::test]
    async fn test_add_element_args() {
        const TEST_ELEMENT_URL: &str = "Test Element Url";

        let proxy = fho::testing::fake_proxy(|req| match req {
            ManagerRequest::ProposeElement { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            ManagerRequest::RemoveElement { .. } => unreachable!(),
        });

        let add_cmd = SessionAddCommand {
            url: TEST_ELEMENT_URL.to_string(),
            interactive: false,
            persist: false,
            name: None,
        };
        let response =
            add_impl(proxy, add_cmd, spawn_ctrl_c_listener(), &mut std::io::stdout()).await;
        assert!(response.is_ok());
    }

    #[fuchsia::test]
    async fn test_add_interactive_element_stop_with_ctrl_c() {
        const TEST_ELEMENT_URL: &str = "Test Element Url";

        let proxy = fho::testing::fake_proxy(move |req| match req {
            ManagerRequest::ProposeElement { responder, .. } => {
                responder.send(Ok(())).unwrap();
            }
            ManagerRequest::RemoveElement { .. } => unreachable!(),
        });

        let add_cmd = SessionAddCommand {
            url: TEST_ELEMENT_URL.to_string(),
            interactive: true,
            persist: false,
            name: None,
        };
        let (ctrl_c_sender, ctrl_c_receiver) = oneshot::channel();
        let mut stdout = std::io::stdout();
        let mut add_fut =
            Box::pin(add_impl(proxy, add_cmd, ctrl_c_receiver.map(|_| ()), &mut stdout));

        assert!(poll!(&mut add_fut).is_pending(), "add should yield until ctrl+c");

        // Send ctrl+c so add will exit.
        ctrl_c_sender.send(()).unwrap();
        let result = add_fut.await;
        assert!(result.is_ok());
    }

    #[fuchsia::test]
    async fn test_add_element_with_persist_and_name() {
        const TEST_ELEMENT_URL: &str = "Test Element Url";

        let proxy = fho::testing::fake_proxy(|req| match req {
            ManagerRequest::ProposeElement { spec, responder, .. } => {
                assert_eq!(spec.component_url.unwrap(), TEST_ELEMENT_URL.to_string());
                let mut got_name = false;
                let mut got_persist = false;
                if let Some(annotations) = spec.annotations {
                    for annotation in annotations {
                        assert_eq!(annotation.key.namespace, felement::MANAGER_NAMESPACE);
                        if annotation.key.value == felement::ANNOTATION_KEY_NAME {
                            assert_matches!(annotation.value,
                                            felement::AnnotationValue::Text(t) if t == "foo");
                            got_name = true;
                        } else if annotation.key.value == felement::ANNOTATION_KEY_PERSIST_ELEMENT {
                            assert_matches!(annotation.value,
                                            felement::AnnotationValue::Text(t) if t == "");
                            got_persist = true;
                        }
                    }
                }
                assert!(got_name && got_persist);
                let _ = responder.send(Ok(()));
            }
            ManagerRequest::RemoveElement { .. } => unreachable!(),
        });

        let add_cmd = SessionAddCommand {
            url: TEST_ELEMENT_URL.to_string(),
            interactive: false,
            persist: true,
            name: Some("foo".to_string()),
        };
        let response =
            add_impl(proxy, add_cmd, spawn_ctrl_c_listener(), &mut std::io::stdout()).await;
        assert!(response.is_ok());
    }
}
