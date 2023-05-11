// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, bail, format_err},
    banjo_fuchsia_wlan_common as banjo_common, fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_sme as fidl_sme, fidl_fuchsia_wlan_softmac as fidl_softmac,
    fuchsia_async as fasync,
    fuchsia_inspect::{self, Inspector},
    fuchsia_inspect_contrib::auto_persist,
    fuchsia_zircon as zx,
    futures::{
        channel::{mpsc, oneshot},
        Future, StreamExt,
    },
    rand,
    std::pin::Pin,
    tracing::{error, info},
    wlan_common::hasher::WlanHasher,
    wlan_mlme::{
        buffer::BufferProvider,
        device::{Device, DeviceInterface, DeviceOps, WlanSoftmacIfcProtocol},
    },
    wlan_sme::{self, serve::create_sme},
};

pub struct WlanSoftmacHandle {
    driver_event_sink: mpsc::UnboundedSender<DriverEvent>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for WlanSoftmacHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("WlanSoftmacHandle").finish()
    }
}

pub type DriverEvent = wlan_mlme::DriverEvent;

impl WlanSoftmacHandle {
    pub fn stop(&mut self) {
        if let Err(e) = self.driver_event_sink.unbounded_send(DriverEvent::Stop) {
            error!("Failed to signal WlanSoftmac main loop thread to stop: {}", e);
        }
        if let Some(join_handle) = self.join_handle.take() {
            if let Err(e) = join_handle.join() {
                error!("WlanSoftmac main loop thread panicked: {:?}", e);
            }
        }
    }

    pub fn delete(mut self) {
        if self.join_handle.is_some() {
            error!("Called delete on WlanSoftmacHandle without first calling stop");
            self.stop();
        }
    }

    pub fn queue_eth_frame_tx(&mut self, bytes: Vec<u8>) -> Result<(), anyhow::Error> {
        self.driver_event_sink
            .unbounded_send(DriverEvent::EthFrameTx { bytes })
            .map_err(|e| e.into())
    }
}

pub fn start_wlansoftmac(
    device: DeviceInterface,
    buf_provider: BufferProvider,
    wlan_softmac_bridge_proxy_raw_handle: fuchsia_zircon::sys::zx_handle_t,
) -> Result<WlanSoftmacHandle, anyhow::Error> {
    let wlan_softmac_bridge_proxy = {
        let handle = unsafe { fidl::Handle::from_raw(wlan_softmac_bridge_proxy_raw_handle) };
        let channel = fidl::Channel::from(handle);
        fidl_softmac::WlanSoftmacBridgeSynchronousProxy::new(channel)
    };
    let mut executor = fasync::LocalExecutor::new();
    executor.run_singlethreaded(start_wlansoftmac_async(
        Device::new(device, wlan_softmac_bridge_proxy),
        buf_provider,
    ))
}

const INSPECT_VMO_SIZE_BYTES: usize = 1000 * 1024;

/// This is a helper function for running wlansoftmac inside a test. For non-test
/// use cases, it should generally be invoked via `start_wlansoftmac`.
async fn start_wlansoftmac_async<D: DeviceOps + Send + 'static>(
    device: D,
    buf_provider: BufferProvider,
) -> Result<WlanSoftmacHandle, anyhow::Error> {
    let (driver_event_sink, driver_event_stream) = mpsc::unbounded();
    let (startup_sender, startup_receiver) = oneshot::channel();
    let inspector =
        Inspector::new(fuchsia_inspect::InspectorConfig::default().size(INSPECT_VMO_SIZE_BYTES));
    let inspect_usme_node = inspector.root().create_child("usme");

    let driver_event_sink_clone = driver_event_sink.clone();
    info!("Spawning wlansoftmac main loop thread.");

    let join_handle = std::thread::spawn(move || {
        let mut executor = fasync::LocalExecutor::new();
        let future = wlansoftmac_thread(
            device,
            buf_provider,
            driver_event_sink_clone,
            driver_event_stream,
            inspector,
            inspect_usme_node,
            startup_sender,
        );
        executor.run_singlethreaded(future);
    });

    match startup_receiver.await.map_err(|e| anyhow::Error::from(e)) {
        Ok(Ok(())) => Ok(WlanSoftmacHandle { driver_event_sink, join_handle: Some(join_handle) }),
        Err(err) | Ok(Err(err)) => match join_handle.join() {
            Ok(()) => bail!("Failed to start the wlansoftmac event loop: {:?}", err),
            Err(panic_err) => {
                bail!("wlansoftmac event loop failed and then panicked: {}, {:?}", err, panic_err)
            }
        },
    }
}

async fn wlansoftmac_thread<D: DeviceOps>(
    mut device: D,
    buf_provider: BufferProvider,
    driver_event_sink: mpsc::UnboundedSender<DriverEvent>,
    driver_event_stream: mpsc::UnboundedReceiver<DriverEvent>,
    inspector: Inspector,
    inspect_usme_node: fuchsia_inspect::Node,
    startup_sender: oneshot::Sender<Result<(), anyhow::Error>>,
) {
    let mut driver_event_sink = wlan_mlme::DriverEventSink(driver_event_sink);

    let ifc = WlanSoftmacIfcProtocol::new(&mut driver_event_sink);

    // Indicate to the vendor driver that we can start sending and receiving
    // info. Any messages received from the driver before we start our SME will
    // be safely buffered in our driver_event_sink.
    // Note that device.start will copy relevant fields out of ifc, so dropping
    // it after this is fine. The returned value is the MLME server end of the
    // channel wlanmevicemonitor created to connect MLME and SME.
    let usme_bootstrap_handle_via_iface_creation = match device.start(&ifc) {
        Ok(handle) => handle,
        Err(e) => {
            // Failure to unwrap indicates a critical failure in the driver init thread.
            startup_sender.send(Err(format_err!("device.start failed: {}", e))).unwrap();
            return;
        }
    };
    let channel = zx::Channel::from(usme_bootstrap_handle_via_iface_creation);
    let server = fidl::endpoints::ServerEnd::<fidl_sme::UsmeBootstrapMarker>::new(channel);
    let (mut usme_bootstrap_stream, _usme_bootstrap_control_handle) =
        match server.into_stream_and_control_handle() {
            Ok(res) => res,
            Err(e) => {
                // Failure to unwrap indicates a critical failure in the driver init thread.
                startup_sender
                    .send(Err(format_err!("Failed to get usme bootstrap stream: {}", e)))
                    .unwrap();
                return;
            }
        };

    let (generic_sme_server, legacy_privacy_support, responder) = match usme_bootstrap_stream
        .next()
        .await
    {
        Some(Ok(fidl_sme::UsmeBootstrapRequest::Start {
            generic_sme_server,
            legacy_privacy_support,
            responder,
            ..
        })) => (generic_sme_server, legacy_privacy_support, responder),
        Some(Err(e)) => {
            startup_sender.send(Err(format_err!("USME bootstrap stream failed: {}", e))).unwrap();
            return;
        }
        None => {
            startup_sender.send(Err(format_err!("USME bootstrap stream terminated"))).unwrap();
            return;
        }
    };
    let inspect_vmo = match inspector.duplicate_vmo() {
        Some(vmo) => vmo,
        None => {
            startup_sender.send(Err(format_err!("Failed to duplicate inspect VMO"))).unwrap();
            return;
        }
    };
    if let Err(e) = responder.send(inspect_vmo).into() {
        startup_sender
            .send(Err(format_err!("Failed to respond to USME bootstrap: {}", e)))
            .unwrap();
        return;
    }
    let generic_sme_stream = match generic_sme_server.into_stream() {
        Ok(stream) => stream,
        Err(e) => {
            startup_sender
                .send(Err(format_err!("Failed to get generic SME stream: {}", e)))
                .unwrap();
            return;
        }
    };

    let softmac_info = device.wlan_softmac_query_response();
    let device_info = match (&softmac_info).try_into() {
        Ok(info) => info,
        Err(e) => {
            startup_sender.send(Err(format_err!("Failed to get MLME device info: {}", e))).unwrap();
            return;
        }
    };

    let mac_sublayer_support =
        match wlan_mlme::convert_ddk_mac_sublayer_support(device.mac_sublayer_support()) {
            Ok(s) => {
                if s.device.mac_implementation_type != fidl_common::MacImplementationType::Softmac {
                    startup_sender
                        .send(Err(format_err!(
                            "Wrong MAC implementation type: {:?}",
                            s.device.mac_implementation_type
                        )))
                        .unwrap();
                    return;
                }
                s
            }
            Err(e) => {
                startup_sender
                    .send(Err(format_err!("Failed to parse device mac sublayer support: {}", e)))
                    .unwrap();
                return;
            }
        };
    let security_support = match wlan_mlme::convert_ddk_security_support(device.security_support())
    {
        Ok(s) => s,
        Err(e) => {
            startup_sender
                .send(Err(format_err!("Failed to parse device security support: {}", e)))
                .unwrap();
            return;
        }
    };
    let spectrum_management_support = match wlan_mlme::convert_ddk_spectrum_management_support(
        device.spectrum_management_support(),
    ) {
        Ok(s) => s,
        Err(e) => {
            startup_sender
                .send(Err(format_err!("Failed to parse device spectrum management support: {}", e)))
                .unwrap();
            return;
        }
    };

    // According to doc, `rand::random` uses ThreadRng, which is cryptographically secure:
    // https://docs.rs/rand/0.5.0/rand/rngs/struct.ThreadRng.html
    let wlan_hasher = WlanHasher::new(rand::random::<u64>().to_le_bytes());

    // TODO(fxbug.dev/113677): Get persistence working by adding the appropriate configs
    //                         in *.cml files
    let (persistence_proxy, _persistence_server_end) = match fidl::endpoints::create_proxy::<
        fidl_fuchsia_diagnostics_persist::DataPersistenceMarker,
    >() {
        Ok(r) => r,
        Err(e) => {
            startup_sender
                .send(Err(format_err!("Failed to create persistence proxy: {}", e)))
                .unwrap();
            return;
        }
    };
    let (persistence_req_sender, _persistence_req_forwarder_fut) =
        auto_persist::create_persistence_req_sender(persistence_proxy);

    let config = wlan_sme::Config {
        wep_supported: legacy_privacy_support.wep_supported,
        wpa1_supported: legacy_privacy_support.wpa1_supported,
    };

    // TODO(fxbug.dev/126324): The MLME event stream should be moved out
    // of DeviceOps entirely.
    let mlme_event_stream = match device.take_mlme_event_stream() {
        Some(mlme_event_stream) => mlme_event_stream,
        None => {
            startup_sender.send(Err(format_err!("Failed to take MLME event stream."))).unwrap();
            return;
        }
    };
    let (mlme_request_stream, sme_fut) = match create_sme(
        config,
        mlme_event_stream,
        &device_info,
        mac_sublayer_support,
        security_support,
        spectrum_management_support,
        inspect_usme_node,
        wlan_hasher,
        persistence_req_sender,
        generic_sme_stream,
    ) {
        Ok((mlme_request_stream, sme_fut)) => (mlme_request_stream, sme_fut),
        Err(e) => {
            startup_sender.send(Err(format_err!("Failed to create sme: {}", e))).unwrap();
            return;
        }
    };

    let mlme_fut: Pin<Box<dyn Future<Output = ()>>> = match softmac_info.mac_role() {
        banjo_common::WlanMacRole::CLIENT => {
            info!("Running wlansoftmac with client role");
            let config = wlan_mlme::client::ClientConfig {
                ensure_on_channel_time: fasync::Duration::from_millis(500).into_nanos(),
            };
            Box::pin(wlan_mlme::mlme_main_loop::<wlan_mlme::client::ClientMlme<D>>(
                config,
                device,
                buf_provider,
                mlme_request_stream,
                driver_event_stream,
                startup_sender,
            ))
        }
        banjo_common::WlanMacRole::AP => {
            info!("Running wlansoftmac with AP role");
            let config = ieee80211::Bssid(softmac_info.sta_addr());
            Box::pin(wlan_mlme::mlme_main_loop::<wlan_mlme::ap::Ap<D>>(
                config,
                device,
                buf_provider,
                mlme_request_stream,
                driver_event_stream,
                startup_sender,
            ))
        }
        unsupported => {
            error!("Unsupported mac role: {:?}", unsupported);
            return;
        }
    };

    let (_, sme_result) = futures::join!(mlme_fut, sme_fut);
    if let Err(e) = sme_result {
        error!("SME shut down with error: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl::endpoints::Proxy,
        pin_utils::pin_mut,
        std::task::Poll,
        wlan_common::assert_variant,
        wlan_mlme::{self, device::test_utils::FakeDevice},
    };

    fn run_wlansoftmac_setup(
        exec: &mut fasync::TestExecutor,
    ) -> Result<(WlanSoftmacHandle, fidl_sme::GenericSmeProxy), anyhow::Error> {
        run_wlansoftmac_setup_with_device(exec, FakeDevice::new(exec).0)
    }

    fn run_wlansoftmac_setup_with_device(
        exec: &mut fasync::TestExecutor,
        fake_device: FakeDevice,
    ) -> Result<(WlanSoftmacHandle, fidl_sme::GenericSmeProxy), anyhow::Error> {
        let fake_buf_provider = wlan_mlme::buffer::FakeBufferProvider::new();
        let handle_fut = start_wlansoftmac_async(fake_device.clone(), fake_buf_provider);

        pin_mut!(handle_fut);
        assert_variant!(exec.run_until_stalled(&mut handle_fut), Poll::Pending);
        let usme_client_proxy = fake_device
            .state()
            .lock()
            .unwrap()
            .usme_bootstrap_client_end
            .take()
            .unwrap()
            .into_proxy()?;
        let legacy_privacy_support =
            fidl_sme::LegacyPrivacySupport { wep_supported: false, wpa1_supported: false };
        let (generic_sme_proxy, generic_sme_server) =
            fidl::endpoints::create_proxy::<fidl_sme::GenericSmeMarker>()?;

        let start_fut = usme_client_proxy.start(generic_sme_server, &legacy_privacy_support);
        let handle = exec.run_singlethreaded(handle_fut)?;
        exec.run_singlethreaded(start_fut).expect("USME boostrap failed");

        Ok((handle, generic_sme_proxy))
    }

    #[test]
    fn wlansoftmac_delete_no_crash() {
        let mut exec = fasync::TestExecutor::new();
        let (mut handle, _generic_sme_proxy) =
            run_wlansoftmac_setup(&mut exec).expect("Failed to start wlansoftmac");
        handle.stop();
        handle.delete();
    }

    #[test]
    fn wlansoftmac_delete_without_stop_no_crash() {
        let mut exec = fasync::TestExecutor::new();
        let (handle, _generic_sme_proxy) =
            run_wlansoftmac_setup(&mut exec).expect("Failed to start wlansoftmac");
        handle.delete();
    }

    #[test]
    fn wlansoftmac_handle_use_after_stop() {
        let mut exec = fasync::TestExecutor::new();
        let (mut handle, _generic_sme_proxy) =
            run_wlansoftmac_setup(&mut exec).expect("Failed to start wlansoftmac");

        handle
            .queue_eth_frame_tx(vec![0u8; 10])
            .expect("Should be able to queue tx before stopping wlansoftmac");
        handle.stop();
        handle
            .queue_eth_frame_tx(vec![0u8; 10])
            .expect_err("Shouldn't be able to queue tx after stopping wlansoftmac");
    }

    #[test]
    fn generic_sme_stops_on_shutdown() {
        let mut exec = fasync::TestExecutor::new();
        let (mut handle, generic_sme_proxy) =
            run_wlansoftmac_setup(&mut exec).expect("Failed to start wlansoftmac");
        let (sme_telemetry_proxy, sme_telemetry_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");
        let (client_proxy, client_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");
        exec.run_singlethreaded(generic_sme_proxy.get_sme_telemetry(sme_telemetry_server))
            .expect("Generic SME proxy failed")
            .expect("SME telemetry request failed");
        exec.run_singlethreaded(generic_sme_proxy.get_client_sme(client_server))
            .expect("Generic SME proxy failed")
            .expect("Client SME request failed");
        handle.stop();

        // All SME proxies should shutdown.
        assert!(generic_sme_proxy.is_closed());
        assert!(sme_telemetry_proxy.is_closed());
        assert!(client_proxy.is_closed());
    }

    #[test]
    fn wlansoftmac_bad_mac_role_fails_startup_with_bad_features() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device_state.lock().unwrap().mac_sublayer_support.device.is_synthetic = true;
        fake_device_state.lock().unwrap().mac_sublayer_support.device.mac_implementation_type =
            banjo_common::MacImplementationType::FULLMAC;
        run_wlansoftmac_setup_with_device(&mut exec, fake_device)
            .expect_err("Softmac setup should fail");
    }

    #[test]
    fn wlansoftmac_startup_fails_on_bad_bootstrap() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let fake_buf_provider = wlan_mlme::buffer::FakeBufferProvider::new();
        let handle_fut = start_wlansoftmac_async(fake_device.clone(), fake_buf_provider);
        pin_mut!(handle_fut);
        assert_variant!(exec.run_until_stalled(&mut handle_fut), Poll::Pending);
        fake_device_state.lock().unwrap().usme_bootstrap_client_end = None; // Drop the client end.
        exec.run_singlethreaded(handle_fut).expect_err("Bootstrap failure should be fatal");
    }
}
