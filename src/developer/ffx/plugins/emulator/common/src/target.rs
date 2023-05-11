// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_remotecontrol::RemoteControlMarker;
use std::time::Duration;
use timeout::timeout;

/// Makes a call to the TargetCollection (similar to `ffx target show`) to get the target
/// that matches the specified emulator. From the target, the RCS connection is opened.
/// If an RCS connection can be made, it is considered "active".
///
/// The request documentation indicates that the call to OpenTarget will hang until the device
/// responds, possibly indefinitely. We wrap the call in a timeout of 1 second, so this function
/// will not hang indefinitely. If the caller expects the response to take longer (such as during
/// Fuchsia bootup), it's safe to call the function repeatedly with a longer local timeout.
pub async fn is_active(collection_proxy: &ffx::TargetCollectionProxy, name: &str) -> bool {
    let (target_proxy, handle) = fidl::endpoints::create_proxy::<ffx::TargetMarker>().unwrap();
    let target = Some(name.to_string());
    let res = timeout(Duration::from_secs(1), async {
        let open_call_result = collection_proxy
            .open_target(&ffx::TargetQuery { string_matcher: target, ..Default::default() }, handle)
            .await;
        tracing::debug!("open_target result: {:?}", &open_call_result);
        if let Ok(open_result) = open_call_result {
            match open_result {
                Ok(()) => {
                    let (_remote_control_proxy, remote_control_handle) =
                        fidl::endpoints::create_proxy::<RemoteControlMarker>().unwrap();
                    let rcs_call_result =
                        target_proxy.open_remote_control(remote_control_handle).await;
                    tracing::debug!("open_remote_control result: {:?}", &rcs_call_result);
                    match rcs_call_result {
                        Ok(rcs_result) => match rcs_result {
                            Ok(()) => true,
                            Err(_) => false,
                        },
                        Err(_) => false,
                    }
                }
                Err(_) => false,
            }
        } else {
            false
        }
    })
    .await;

    tracing::debug!("returning {:?}", &res);
    return res.unwrap_or_else(|_| false);
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::endpoints::ServerEnd;
    use futures::TryStreamExt;

    // Sets up a TargetCollectionProxy that handles the requests via the callback.
    fn setup_fake_target_collection_proxy<R: 'static>(
        mut handle_request: R,
    ) -> ffx::TargetCollectionProxy
    where
        R: FnMut(
            fidl::endpoints::Request<
                <ffx::TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
            >,
        ),
    {
        let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<
            <ffx::TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                handle_request(req);
            }
        })
        .detach();
        proxy
    }

    // Sets up the handling of OpenTarget requests which are passed to the handler. all others are ignored.
    fn setup_target_collection<F>(open_target_handler: F) -> ffx::TargetCollectionProxy
    where
        F: Fn(ffx::TargetQuery, ServerEnd<ffx::TargetMarker>) -> Result<(), ffx::OpenTargetError>
            + Clone
            + 'static,
    {
        setup_fake_target_collection_proxy(move |req| match req {
            ffx::TargetCollectionRequest::OpenTarget { query, target_handle, responder } => {
                let mut res = (open_target_handler)(query, target_handle);
                responder.send(&mut res).unwrap();
            }
            _ => {}
        })
    }

    // Sets up the Target Request handler. The target_handle is passed in and is the same handle used
    // when responding to OpenTarget.
    fn spawn_target_handler(
        target_handle: ServerEnd<ffx::TargetMarker>,
        handler: impl Fn(ffx::TargetRequest) -> () + 'static,
    ) {
        fuchsia_async::Task::local(async move {
            let mut stream = target_handle.into_stream().unwrap();
            while let Ok(Some(req)) = stream.try_next().await {
                handler(req)
            }
        })
        .detach();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_is_active() {
        // This handles the OpenTarget request. If the target is opened successfully,
        // the TargetRequest handler is set up so TargetRequest::OpenRemoteControlResponder can be
        // handled.
        let handler = |query: ffx::TargetQuery, _handle: ServerEnd<ffx::TargetMarker>| {
            {
                if let Some(string_matcher) = &query.string_matcher {
                    if string_matcher.contains("good_target_id") {
                        // Open the RCS without any problem
                        spawn_target_handler(_handle, |req| match req {
                            ffx::TargetRequest::OpenRemoteControl {
                                responder,
                                remote_control: _,
                            } => {
                                responder.send(&mut Ok(())).unwrap();
                            }
                            r => panic!("unexpected request: {:?}", r),
                        });
                    } else {
                        return Err(ffx::OpenTargetError::TargetNotFound);
                    }
                } else {
                    return Err(ffx::OpenTargetError::QueryAmbiguous);
                }
            }
            Ok(())
        };
        let server = setup_target_collection(handler);
        // The "target" that we expect is "active".
        assert!(is_active(&server, "good_target_id").await);
        // The "target" that we don't expect is "inactive".
        assert!(!is_active(&server, "unknown_target_id").await);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_connection_error_for_rcs() {
        // This handler returns connection refused for open rcs requests.
        let handler = |_query: ffx::TargetQuery, _handle: ServerEnd<ffx::TargetMarker>| {
            {
                // Open the RCS and return connection refused
                spawn_target_handler(_handle, |req| match req {
                    ffx::TargetRequest::OpenRemoteControl { responder, remote_control: _ } => {
                        responder
                            .send(&mut Err(ffx::TargetConnectionError::ConnectionRefused))
                            .unwrap();
                    }
                    r => panic!("unexpected request: {:?}", r),
                });
            }
            Ok(())
        };

        let server = setup_target_collection(handler);
        assert!(!is_active(&server, "target").await);
    }
}
