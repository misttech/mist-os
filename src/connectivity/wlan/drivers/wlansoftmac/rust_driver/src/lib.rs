// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_sme as fidl_sme,
    fidl_fuchsia_wlan_softmac as fidl_softmac, fuchsia_async as fasync,
    fuchsia_inspect::{self, Inspector, Node as InspectNode},
    fuchsia_inspect_contrib::auto_persist,
    fuchsia_trace as ftrace,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{
        channel::{mpsc, oneshot},
        Future, FutureExt, StreamExt,
    },
    std::pin::Pin,
    tracing::{error, info},
    wlan_mlme::{
        buffer::BufferProvider,
        device::{
            completers::{InitCompleter, StopCompleter},
            DeviceOps, WlanSoftmacIfcProtocol,
        },
        DriverEvent,
    },
    wlan_sme::{self, serve::create_sme},
    wlan_trace as wtrace,
};

const INSPECT_VMO_SIZE_BYTES: usize = 1000 * 1024;

pub struct WlanSoftmacHandle(mpsc::UnboundedSender<DriverEvent>);

impl std::fmt::Debug for WlanSoftmacHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("WlanSoftmacHandle").finish()
    }
}

impl WlanSoftmacHandle {
    pub fn stop(mut self, stop_completer: StopCompleter) {
        wtrace::duration!(c"WlanSoftmacHandle::stop");
        let driver_event_sink = &mut self.0;
        if let Err(e) = driver_event_sink.unbounded_send(DriverEvent::Stop(stop_completer)) {
            error!("Failed to signal WlanSoftmac main loop thread to stop: {}", e);
            if let DriverEvent::Stop(stop_completer) = e.into_inner() {
                stop_completer.complete();
            } else {
                unreachable!();
            }
        }
        driver_event_sink.disconnect();
    }

    pub fn queue_eth_frame_tx(
        &mut self,
        bytes: Vec<u8>,
        async_id: ftrace::Id,
    ) -> Result<(), zx::Status> {
        wtrace::duration!(c"WlanSoftmacHandle::queue_eth_frame_tx");
        let driver_event_sink = &mut self.0;
        driver_event_sink.unbounded_send(DriverEvent::EthFrameTx { bytes, async_id }).map_err(|e| {
            error!("Failed to queue ethernet frame: {:?}", e);
            zx::Status::INTERNAL
        })
    }
}

/// Run the Rust portion of wlansoftmac which includes the following three futures:
///
///   - WlanSoftmacIfcBridge server
///   - MLME server
///   - SME server
///
/// The WlanSoftmacIfcBridge server future executes on a parallel thread because otherwise synchronous calls
/// from the MLME server into the vendor driver could deadlock if the vendor driver calls a
/// WlanSoftmacIfcBridge method before returning from a synchronous call. For example, when the MLME server
/// synchronously calls WlanSoftmac.StartActiveScan(), the vendor driver may call
/// WlanSoftmacIfc.NotifyScanComplete() before returning from WlanSoftmac.StartActiveScan(). This can occur
/// when the scan request results in immediate cancellation despite the request having valid arguments.
///
/// This function calls init_completer() when either MLME initialization completes successfully or an error
/// occurs before MLME initialization completes. The Ok() value passed by init_completer() is a
/// WlanSoftmacHandle which contains the FFI for calling WlanSoftmacIfc methods from the C++ portion of
/// wlansoftmac.
///
/// The return value of this function is distinct from the value passed in init_completer(). This
/// function returns in one of four cases:
///
///   - An error occurred during initialization.
///   - An error occurred while running.
///   - An error occurred during shutdown.
///   - Shutdown completed successfully.
///
/// Generally, errors during initializations will be returned immediately after this function calls init_completer()
/// with the same error. Later errors can be returned after this functoin calls init_completer().
pub async fn start_and_serve<D: DeviceOps + 'static>(
    init_completer: impl FnOnce(Result<WlanSoftmacHandle, zx::Status>) + Send + 'static,
    device: D,
    buf_provider: BufferProvider,
) -> Result<(), zx::Status> {
    wtrace::duration_begin_scope!(c"rust_driver::start_and_serve");
    let (driver_event_sink, driver_event_stream) = mpsc::unbounded();
    let softmac_handle_sink = driver_event_sink.clone();
    let init_completer = InitCompleter::new(move |result: Result<(), zx::Status>| {
        init_completer(result.map(|()| WlanSoftmacHandle(softmac_handle_sink)))
    });

    let (mlme_init_sender, mlme_init_receiver) = oneshot::channel();
    let StartedDriver { mlme: mlme_fut, sme: sme_fut } =
        match start(mlme_init_sender, driver_event_sink, driver_event_stream, device, buf_provider)
            .await
        {
            Err(status) => {
                init_completer.complete(Err(status));
                return Err(status);
            }
            Ok(x) => x,
        };

    serve(init_completer, mlme_init_receiver, mlme_fut, sme_fut).await
}

#[derive(Debug)]
struct StartedDriver<F1, F2> {
    pub mlme: F1,
    pub sme: F2,
}

/// Start the bridged wlansoftmac driver by creating components to run two futures:
///
///   - MLME server
///   - SME server
///
/// This function will use the provided |device| to make various calls into the vendor driver necessary to
/// configure and create the components to run the futures.
async fn start<D: DeviceOps + 'static>(
    mlme_init_sender: oneshot::Sender<Result<(), zx::Status>>,
    driver_event_sink: mpsc::UnboundedSender<DriverEvent>,
    driver_event_stream: mpsc::UnboundedReceiver<DriverEvent>,
    mut device: D,
    buf_provider: BufferProvider,
) -> Result<
    StartedDriver<
        Pin<Box<dyn Future<Output = Result<(), Error>>>>,
        Pin<Box<impl Future<Output = Result<(), Error>>>>,
    >,
    zx::Status,
> {
    wtrace::duration!(c"rust_driver::start");

    // Create WlanSoftmacIfcProtocol FFI and WlanSoftmacIfcBridge client to bootstrap USME.
    let mut mlme_sink = wlan_mlme::DriverEventSink(driver_event_sink);
    let softmac_ifc_ffi = WlanSoftmacIfcProtocol::new(&mut mlme_sink);

    // Bootstrap USME
    let BootstrappedGenericSme { generic_sme_request_stream, legacy_privacy_support, inspect_node } =
        bootstrap_generic_sme(&mut device, &softmac_ifc_ffi).await?;

    // Make a series of queries to gather device information from the vendor driver.
    let softmac_info = device.wlan_softmac_query_response()?;
    let sta_addr = softmac_info.sta_addr;
    let device_info = match wlan_mlme::mlme_device_info_from_softmac(softmac_info) {
        Ok(info) => info,
        Err(e) => {
            error!("Failed to get MLME device info: {}", e);
            return Err(zx::Status::INTERNAL);
        }
    };

    let mac_sublayer_support = device.mac_sublayer_support()?;
    let mac_implementation_type = &mac_sublayer_support.device.mac_implementation_type;
    if *mac_implementation_type != fidl_common::MacImplementationType::Softmac {
        error!("Wrong MAC implementation type: {:?}", mac_implementation_type);
        return Err(zx::Status::INTERNAL);
    }
    let security_support = device.security_support()?;
    let spectrum_management_support = device.spectrum_management_support()?;

    // TODO(https://fxbug.dev/42064968): Get persistence working by adding the appropriate configs
    //                         in *.cml files
    let (persistence_proxy, _persistence_server_end) = match fidl::endpoints::create_proxy::<
        fidl_fuchsia_diagnostics_persist::DataPersistenceMarker,
    >() {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to create persistence proxy: {}", e);
            return Err(zx::Status::INTERNAL);
        }
    };
    let (persistence_req_sender, _persistence_req_forwarder_fut) =
        auto_persist::create_persistence_req_sender(persistence_proxy);

    let config = wlan_sme::Config {
        wep_supported: legacy_privacy_support.wep_supported,
        wpa1_supported: legacy_privacy_support.wpa1_supported,
    };

    // TODO(https://fxbug.dev/42077094): The MLME event stream should be moved out of DeviceOps entirely.
    let mlme_event_stream = match device.take_mlme_event_stream() {
        Some(mlme_event_stream) => mlme_event_stream,
        None => {
            error!("Failed to take MLME event stream.");
            return Err(zx::Status::INTERNAL);
        }
    };

    // Create an SME future to serve
    let (mlme_request_stream, sme_fut) = match create_sme(
        config,
        mlme_event_stream,
        &device_info,
        mac_sublayer_support,
        security_support,
        spectrum_management_support,
        inspect_node,
        persistence_req_sender,
        generic_sme_request_stream,
    ) {
        Ok((mlme_request_stream, sme_fut)) => (mlme_request_stream, sme_fut),
        Err(e) => {
            error!("Failed to create sme: {}", e);
            return Err(zx::Status::INTERNAL);
        }
    };

    // Create an MLME future to serve
    let mlme_fut: Pin<Box<dyn Future<Output = Result<(), Error>>>> = match device_info.role {
        fidl_common::WlanMacRole::Client => {
            info!("Running wlansoftmac with client role");
            let config = wlan_mlme::client::ClientConfig {
                ensure_on_channel_time: fasync::Duration::from_millis(500).into_nanos(),
            };
            Box::pin(wlan_mlme::mlme_main_loop::<wlan_mlme::client::ClientMlme<D>>(
                mlme_init_sender,
                config,
                device,
                buf_provider,
                mlme_request_stream,
                driver_event_stream,
            ))
        }
        fidl_common::WlanMacRole::Ap => {
            info!("Running wlansoftmac with AP role");
            let sta_addr = match sta_addr {
                Some(sta_addr) => sta_addr,
                None => {
                    error!("Driver provided no STA address.");
                    return Err(zx::Status::INTERNAL);
                }
            };
            let config = ieee80211::Bssid::from(sta_addr);
            Box::pin(wlan_mlme::mlme_main_loop::<wlan_mlme::ap::Ap<D>>(
                mlme_init_sender,
                config,
                device,
                buf_provider,
                mlme_request_stream,
                driver_event_stream,
            ))
        }
        unsupported => {
            error!("Unsupported mac role: {:?}", unsupported);
            return Err(zx::Status::INTERNAL);
        }
    };

    Ok(StartedDriver { mlme: mlme_fut, sme: sme_fut })
}

/// Await on futures hosting the following three servers:
///
///   - WlanSoftmacIfcBridge server
///   - MLME server
///   - SME server
///
/// The WlanSoftmacIfcBridge server runs on a parallel thread but will be shut down before this function
/// returns. This is true even if this function exits with an error.
///
/// Upon receiving a DriverEvent::Stop, the MLME server will shut down first. Then this function will await
/// the completion of WlanSoftmacIfcBridge server and SME server. Both will shut down as a consequence of
/// MLME server shut down.
async fn serve<InitFn>(
    init_completer: InitCompleter<InitFn>,
    mlme_init_receiver: oneshot::Receiver<Result<(), zx::Status>>,
    mlme_fut: Pin<Box<dyn Future<Output = Result<(), Error>>>>,
    sme_fut: Pin<Box<impl Future<Output = Result<(), Error>>>>,
) -> Result<(), zx::Status>
where
    InitFn: FnOnce(Result<(), zx::Status>) + Send,
{
    wtrace::duration_begin_scope!(c"rust_driver::serve");

    let mut mlme_fut = mlme_fut.fuse();
    let mut sme_fut = sme_fut.fuse();

    // oneshot::Receiver implements FusedFuture incorrectly, so we must call .fuse()
    // to get the right behavior in the select!().
    //
    // See https://github.com/rust-lang/futures-rs/issues/2455 for more details.
    let mut mlme_init_receiver = mlme_init_receiver.fuse();

    // Run the MLME server and wait for the MLME to signal initialization completion.
    {
        wtrace::duration_begin_scope!(c"initialize MLME");
        futures::select! {
            init_result = mlme_init_receiver => {
                let init_result = match init_result {
                    Ok(x) => x,
                    Err(e) => {
                        error!("mlme_init_receiver interrupted: {}", e);
                        let status = zx::Status::INTERNAL;
                        init_completer.complete(Err(status));
                        return Err(status);
                    }
                };
                match init_result {
                    Ok(()) =>
                        init_completer.complete(Ok(())),
                    Err(status) => {
                        error!("Failed to initialize MLME: {}", status);
                        init_completer.complete(Err(status));
                        return Err(status);
                    }
                }
            },
            mlme_result = mlme_fut => {
                error!("MLME future completed before signaling init_sender: {:?}", mlme_result);
                let status = zx::Status::INTERNAL;
                init_completer.complete(Err(status));
                return Err(status);
            }
        }
    }

    // Run the SME and MLME servers.
    let server_shutdown_result = {
        wtrace::duration_begin_scope!(c"run MLME and SME");
        futures::select! {
            mlme_result = mlme_fut => {
                match mlme_result {
                    Ok(()) => {
                        info!("MLME shut down gracefully.");
                        Ok(())
                    },
                    Err(e) => {
                        error!("MLME shut down with error: {}", e);
                        Err(zx::Status::INTERNAL)
                    }
                }
            }
            sme_result = sme_fut => {
                error!("SME shut down before MLME: {:?}", sme_result);
                Err(zx::Status::INTERNAL)
            }
        }
    };

    // If any future returns an error, return immediately with the same error without waiting
    // for other futures to complete.
    server_shutdown_result?;

    // Since the MLME server is shut down at this point, the SME server will shut down soon because the SME
    // server always shuts down when it loses connection with the MLME server.
    sme_fut.await.map(|()| info!("SME shut down gracefully")).map_err(|e| {
        error!("SME shut down with error: {}", e);
        zx::Status::INTERNAL
    })
}

struct BootstrappedGenericSme {
    pub generic_sme_request_stream: fidl_sme::GenericSmeRequestStream,
    pub legacy_privacy_support: fidl_sme::LegacyPrivacySupport,
    pub inspect_node: InspectNode,
}

/// Call WlanSoftmac.Start() to retrieve the server end of UsmeBootstrap channel and wait
/// for a UsmeBootstrap.Start() message to provide the server end of a GenericSme channel.
///
/// Any errors encountered in this function are fatal for the wlansoftmac driver. Failure to bootstrap
/// GenericSme request stream will result in a driver no other component can communicate with.
async fn bootstrap_generic_sme<D: DeviceOps>(
    device: &mut D,
    softmac_ifc_ffi: &WlanSoftmacIfcProtocol<'_>,
) -> Result<BootstrappedGenericSme, zx::Status> {
    wtrace::duration!(c"rust_driver::bootstrap_generic_sme");

    let (softmac_ifc_bridge_client, _softmac_ifc_bridge_server) =
        fidl::endpoints::create_endpoints::<fidl_softmac::WlanSoftmacIfcBridgeMarker>();

    // Calling WlanSoftmac.Start() indicates to the vendor driver that this driver (wlansoftmac) is ready to
    // receive WlanSoftmacIfc messages. wlansoftmac will buffer all WlanSoftmacIfc messages in an
    // mpsc::UnboundedReceiver<DriverEvent> sink until the MLME server drains them. The C++ portion of
    // wlansoftmac will copy the pointers (to static functions) out of the WlanSoftmacIfcProtocol, so
    // dropping the WlanSoftmacIfcProtocol after calling WlanSoftmac.Start() is safe.
    let usme_bootstrap_handle_via_iface_creation = match device.start(
        softmac_ifc_ffi,
        zx::Handle::from(softmac_ifc_bridge_client.into_channel()).into_raw(),
    ) {
        Ok(handle) => handle,
        Err(status) => {
            error!("Failed to receive a UsmeBootstrap handle: {}", status);
            return Err(status);
        }
    };
    let channel = zx::Channel::from(usme_bootstrap_handle_via_iface_creation);
    let server = fidl::endpoints::ServerEnd::<fidl_sme::UsmeBootstrapMarker>::new(channel);
    let mut usme_bootstrap_stream = match server.into_stream() {
        Ok(res) => res,
        Err(e) => {
            error!("Failed to create a UsmeBootstrap request stream: {}", e);
            return Err(zx::Status::INTERNAL);
        }
    };

    let (generic_sme_server, legacy_privacy_support, responder) =
        match usme_bootstrap_stream.next().await {
            Some(Ok(fidl_sme::UsmeBootstrapRequest::Start {
                generic_sme_server,
                legacy_privacy_support,
                responder,
                ..
            })) => (generic_sme_server, legacy_privacy_support, responder),
            Some(Err(e)) => {
                error!("Received an error on USME bootstrap request stream: {}", e);
                return Err(zx::Status::INTERNAL);
            }
            None => {
                // This is always an error because the SME server should not drop
                // the USME client endpoint until MLME shut down first.
                error!("USME bootstrap stream terminated unexpectedly");
                return Err(zx::Status::INTERNAL);
            }
        };

    let inspector =
        Inspector::new(fuchsia_inspect::InspectorConfig::default().size(INSPECT_VMO_SIZE_BYTES));
    let inspect_node = inspector.root().create_child("usme");

    let inspect_vmo = match inspector.duplicate_vmo() {
        Some(vmo) => vmo,
        None => {
            error!("Failed to duplicate inspect VMO");
            return Err(zx::Status::INTERNAL);
        }
    };
    if let Err(e) = responder.send(inspect_vmo).into() {
        error!("Failed to respond to UsmeBootstrap.Start(): {}", e);
        return Err(zx::Status::INTERNAL);
    }
    let generic_sme_request_stream = match generic_sme_server.into_stream() {
        Ok(stream) => stream,
        Err(e) => {
            error!("Failed to create GenericSme request stream: {}", e);
            return Err(zx::Status::INTERNAL);
        }
    };

    Ok(BootstrappedGenericSme { generic_sme_request_stream, legacy_privacy_support, inspect_node })
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        anyhow::format_err,
        diagnostics_assertions::assert_data_tree,
        fidl::endpoints::Proxy,
        fuchsia_inspect::InspectorConfig,
        futures::{channel::oneshot, stream::FuturesUnordered, task::Poll},
        pin_utils::pin_mut,
        std::sync::Arc,
        test_case::test_case,
        wlan_common::assert_variant,
        wlan_mlme::{
            self,
            device::test_utils::{FakeDevice, FakeDeviceConfig},
        },
        zx::Vmo,
    };

    #[test]
    fn bootstrap_generic_sme_fails_to_retrieve_usme_bootstrap_handle() {
        let mut exec = fasync::TestExecutor::new();
        let (mut fake_device, _fake_device_state) = FakeDevice::new_with_config(
            &exec,
            FakeDeviceConfig::default().with_mock_start_result(Err(zx::Status::INTERRUPTED_RETRY)),
        );

        // Create WlanSoftmacIfcProtocol FFI and WlanSoftmacIfcBridge client to bootstrap USME.
        let (driver_event_sink, _driver_event_stream) = mpsc::unbounded();
        let mut mlme_sink = wlan_mlme::DriverEventSink(driver_event_sink);
        let softmac_ifc_ffi = WlanSoftmacIfcProtocol::new(&mut mlme_sink);

        let fut = bootstrap_generic_sme(&mut fake_device, &softmac_ifc_ffi);
        pin_mut!(fut);
        match exec.run_until_stalled(&mut fut) {
            Poll::Ready(Err(zx::Status::INTERRUPTED_RETRY)) => (),
            Poll::Ready(Err(status)) => panic!("Failed with wrong status: {}", status),
            Poll::Ready(Ok(_)) => panic!("Succeeded unexpectedly"),
            Poll::Pending => panic!("bootstrap_generic_sme() unexpectedly stalled"),
        }
    }

    #[test]
    fn boostrap_generic_sme_fails_on_error_from_bootstrap_stream() {
        let mut exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) =
            FakeDevice::new_with_config(&exec, FakeDeviceConfig::default());

        // Create WlanSoftmacIfcProtocol FFI and WlanSoftmacIfcBridge client to bootstrap USME.
        let (driver_event_sink, _driver_event_stream) = mpsc::unbounded();
        let mut mlme_sink = wlan_mlme::DriverEventSink(driver_event_sink);
        let softmac_ifc_ffi = WlanSoftmacIfcProtocol::new(&mut mlme_sink);

        let fut = bootstrap_generic_sme(&mut fake_device, &softmac_ifc_ffi);
        pin_mut!(fut);
        assert!(matches!(exec.run_until_stalled(&mut fut), Poll::Pending));

        // Write an invalid FIDL message to the USME bootstrap channel.
        let usme_bootstrap_channel =
            fake_device_state.lock().usme_bootstrap_client_end.take().unwrap().into_channel();
        usme_bootstrap_channel.write(&[], &mut []).unwrap();

        assert!(matches!(exec.run_until_stalled(&mut fut), Poll::Ready(Err(zx::Status::INTERNAL))));
    }

    #[test]
    fn boostrap_generic_sme_fails_on_closed_bootstrap_stream() {
        let mut exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) =
            FakeDevice::new_with_config(&exec, FakeDeviceConfig::default());

        // Create WlanSoftmacIfcProtocol FFI and WlanSoftmacIfcBridge client to bootstrap USME.
        let (driver_event_sink, _driver_event_stream) = mpsc::unbounded();
        let mut mlme_sink = wlan_mlme::DriverEventSink(driver_event_sink);
        let softmac_ifc_ffi = WlanSoftmacIfcProtocol::new(&mut mlme_sink);

        let fut = bootstrap_generic_sme(&mut fake_device, &softmac_ifc_ffi);
        pin_mut!(fut);
        assert!(matches!(exec.run_until_stalled(&mut fut), Poll::Pending));

        // Drop the client end of USME bootstrap channel.
        let _ = fake_device_state.lock().usme_bootstrap_client_end.take().unwrap();

        assert!(matches!(exec.run_until_stalled(&mut fut), Poll::Ready(Err(zx::Status::INTERNAL))));
    }

    #[test]
    fn boostrap_generic_sme_succeeds() {
        let mut exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) =
            FakeDevice::new_with_config(&exec, FakeDeviceConfig::default());

        // Create WlanSoftmacIfcProtocol FFI and WlanSoftmacIfcBridge client to bootstrap USME.
        let (driver_event_sink, _driver_event_stream) = mpsc::unbounded();
        let mut mlme_sink = wlan_mlme::DriverEventSink(driver_event_sink);
        let softmac_ifc_ffi = WlanSoftmacIfcProtocol::new(&mut mlme_sink);

        let bootstrap_generic_sme_fut = bootstrap_generic_sme(&mut fake_device, &softmac_ifc_ffi);
        pin_mut!(bootstrap_generic_sme_fut);
        assert!(matches!(exec.run_until_stalled(&mut bootstrap_generic_sme_fut), Poll::Pending));

        let usme_bootstrap_proxy = fake_device_state
            .lock()
            .usme_bootstrap_client_end
            .take()
            .unwrap()
            .into_proxy()
            .unwrap();

        let sent_legacy_privacy_support =
            fidl_sme::LegacyPrivacySupport { wep_supported: false, wpa1_supported: false };
        let (generic_sme_proxy, generic_sme_server) =
            fidl::endpoints::create_proxy::<fidl_sme::GenericSmeMarker>().unwrap();
        let inspect_vmo_fut =
            usme_bootstrap_proxy.start(generic_sme_server, &sent_legacy_privacy_support);
        pin_mut!(inspect_vmo_fut);
        assert!(matches!(exec.run_until_stalled(&mut inspect_vmo_fut), Poll::Pending));

        let BootstrappedGenericSme {
            mut generic_sme_request_stream,
            legacy_privacy_support: received_legacy_privacy_support,
            inspect_node,
        } = match exec.run_until_stalled(&mut bootstrap_generic_sme_fut) {
            Poll::Pending => panic!("bootstrap_generic_sme_fut() did not complete!"),
            Poll::Ready(x) => x.unwrap(),
        };
        let inspect_vmo = match exec.run_until_stalled(&mut inspect_vmo_fut) {
            Poll::Pending => panic!("Failed to receive an inspect VMO."),
            Poll::Ready(x) => x.unwrap(),
        };

        // Send a GenericSme.Query() to check the generic_sme_proxy
        // and generic_sme_stream are connected.
        let query_fut = generic_sme_proxy.query();
        pin_mut!(query_fut);
        assert!(matches!(exec.run_until_stalled(&mut query_fut), Poll::Pending));
        let next_generic_sme_request_fut = generic_sme_request_stream.next();
        pin_mut!(next_generic_sme_request_fut);
        assert!(matches!(
            exec.run_until_stalled(&mut next_generic_sme_request_fut),
            Poll::Ready(Some(Ok(fidl_sme::GenericSmeRequest::Query { .. })))
        ));

        assert_eq!(received_legacy_privacy_support, sent_legacy_privacy_support);

        // Add a child node through inspect_node and verify the node appears inspect_vmo.
        let inspect_vmo = Arc::new(inspect_vmo);
        let inspector = Inspector::new(InspectorConfig::default().vmo(inspect_vmo));
        let _a = inspect_node.create_child("a");
        assert_data_tree!(inspector, root: {
            usme: { a: {} },
        });
    }

    struct StartTestHarness {
        #[allow(dead_code)]
        pub mlme_init_receiver: Pin<Box<oneshot::Receiver<Result<(), zx::Status>>>>,
        #[allow(dead_code)]
        pub driver_event_sink: mpsc::UnboundedSender<DriverEvent>,
    }

    impl StartTestHarness {
        fn new(
            fake_device: FakeDevice,
        ) -> (
            Pin<
                Box<
                    impl Future<
                        Output = Result<
                            StartedDriver<
                                Pin<Box<dyn Future<Output = Result<(), Error>>>>,
                                Pin<Box<impl Future<Output = Result<(), Error>>>>,
                            >,
                            zx::Status,
                        >,
                    >,
                >,
            >,
            StartTestHarness,
        ) {
            let (mlme_init_sender, mlme_init_receiver) = oneshot::channel();
            let (driver_event_sink, driver_event_stream) = mpsc::unbounded();
            let fake_buf_provider = wlan_mlme::buffer::FakeBufferProvider::new();

            (
                Box::pin(start(
                    mlme_init_sender,
                    driver_event_sink.clone(),
                    driver_event_stream,
                    fake_device,
                    fake_buf_provider,
                )),
                StartTestHarness {
                    mlme_init_receiver: Box::pin(mlme_init_receiver),
                    driver_event_sink,
                },
            )
        }
    }

    #[test]
    fn start_fails_on_bad_bootstrap() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, _fake_device_state) = FakeDevice::new_with_config(
            &exec,
            FakeDeviceConfig::default().with_mock_start_result(Err(zx::Status::INTERRUPTED_RETRY)),
        );
        let (mut start_fut, _harness) = StartTestHarness::new(fake_device);

        assert!(matches!(
            exec.run_until_stalled(&mut start_fut),
            Poll::Ready(Err(zx::Status::INTERRUPTED_RETRY))
        ));
    }

    fn bootstrap_generic_sme_proxy_and_inspect_vmo(
        usme_bootstrap_client_end: fidl::endpoints::ClientEnd<fidl_sme::UsmeBootstrapMarker>,
    ) -> (fidl_sme::GenericSmeProxy, impl Future<Output = Result<Vmo, fidl::Error>>) {
        let usme_client_proxy = usme_bootstrap_client_end
            .into_proxy()
            .expect("Failed to set up the USME client proxy.");

        let legacy_privacy_support =
            fidl_sme::LegacyPrivacySupport { wep_supported: false, wpa1_supported: false };
        let (generic_sme_proxy, generic_sme_server) =
            fidl::endpoints::create_proxy::<fidl_sme::GenericSmeMarker>().unwrap();
        (generic_sme_proxy, usme_client_proxy.start(generic_sme_server, &legacy_privacy_support))
    }

    #[test_case(FakeDeviceConfig::default().with_mock_query_response(Err(zx::Status::IO_DATA_INTEGRITY)), zx::Status::IO_DATA_INTEGRITY)]
    #[test_case(FakeDeviceConfig::default().with_mock_mac_sublayer_support(Err(zx::Status::IO_DATA_INTEGRITY)), zx::Status::IO_DATA_INTEGRITY)]
    #[test_case(FakeDeviceConfig::default().with_mock_mac_implementation_type(fidl_common::MacImplementationType::Fullmac), zx::Status::INTERNAL)]
    #[test_case(FakeDeviceConfig::default().with_mock_security_support(Err(zx::Status::IO_DATA_INTEGRITY)), zx::Status::IO_DATA_INTEGRITY)]
    #[test_case(FakeDeviceConfig::default().with_mock_spectrum_management_support(Err(zx::Status::IO_DATA_INTEGRITY)), zx::Status::IO_DATA_INTEGRITY)]
    #[test_case(FakeDeviceConfig::default().with_mock_mac_role(fidl_common::WlanMacRole::__SourceBreaking { unknown_ordinal: 0 }), zx::Status::INTERNAL)]
    fn start_fails_on_query_error(
        fake_device_config: FakeDeviceConfig,
        expected_status: zx::Status,
    ) {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) =
            FakeDevice::new_with_config(&exec, fake_device_config);
        let (mut start_fut, _harness) = StartTestHarness::new(fake_device);

        let usme_bootstrap_client_end =
            fake_device_state.lock().usme_bootstrap_client_end.take().unwrap();
        let (_generic_sme_proxy, _inspect_vmo_fut) =
            bootstrap_generic_sme_proxy_and_inspect_vmo(usme_bootstrap_client_end);

        match exec.run_until_stalled(&mut start_fut) {
            Poll::Ready(Err(status)) => assert_eq!(status, expected_status),
            Poll::Pending => panic!("start_fut still pending!"),
            Poll::Ready(Ok(_)) => panic!("start_fut completed with Ok value"),
        }
    }

    #[test]
    fn start_fail_on_dropped_mlme_event_stream() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let (mut start_fut, _harness) = StartTestHarness::new(fake_device);

        let usme_bootstrap_client_end =
            fake_device_state.lock().usme_bootstrap_client_end.take().unwrap();
        let (_generic_sme_proxy, _inspect_vmo_fut) =
            bootstrap_generic_sme_proxy_and_inspect_vmo(usme_bootstrap_client_end);

        let _ = fake_device_state.lock().mlme_event_stream.take();
        match exec.run_until_stalled(&mut start_fut) {
            Poll::Ready(Err(status)) => assert_eq!(status, zx::Status::INTERNAL),
            Poll::Pending => panic!("start_fut still pending!"),
            Poll::Ready(Ok(_)) => panic!("start_fut completed with Ok value"),
        }
    }

    #[test]
    fn start_succeeds() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new_with_config(
            &exec,
            FakeDeviceConfig::default()
                .with_mock_sta_addr([2u8; 6])
                .with_mock_mac_role(fidl_common::WlanMacRole::Client),
        );
        let (mut start_fut, mut harness) = StartTestHarness::new(fake_device);

        let usme_bootstrap_client_end =
            fake_device_state.lock().usme_bootstrap_client_end.take().unwrap();
        let (generic_sme_proxy, _inspect_vmo_fut) =
            bootstrap_generic_sme_proxy_and_inspect_vmo(usme_bootstrap_client_end);

        let StartedDriver { mlme: mut mlme_fut, sme: sme_fut } =
            match exec.run_until_stalled(&mut start_fut) {
                Poll::Ready(Ok(x)) => x,
                Poll::Ready(Err(status)) => {
                    panic!("start_fut unexpectedly failed; {}", status)
                }
                Poll::Pending => panic!("start_fut still pending!"),
            };

        assert_variant!(exec.run_until_stalled(&mut mlme_fut), Poll::Pending);
        assert!(matches!(
            exec.run_until_stalled(&mut harness.mlme_init_receiver),
            Poll::Ready(Ok(Ok(())))
        ));

        let resp_fut = generic_sme_proxy.query();
        pin_mut!(resp_fut);
        assert_variant!(exec.run_until_stalled(&mut resp_fut), Poll::Pending);

        let sme_and_mlme_fut = [sme_fut, mlme_fut].into_iter().collect::<FuturesUnordered<_>>();
        pin_mut!(sme_and_mlme_fut);
        assert!(matches!(exec.run_until_stalled(&mut sme_and_mlme_fut.next()), Poll::Pending));

        assert!(matches!(
            exec.run_until_stalled(&mut resp_fut),
            Poll::Ready(Ok(fidl_sme::GenericSmeQuery {
                role: fidl_common::WlanMacRole::Client,
                sta_addr: [2, 2, 2, 2, 2, 2],
            }))
        ));
    }

    struct ServeTestHarness {
        pub init_complete_receiver: Pin<Box<oneshot::Receiver<Result<(), zx::Status>>>>,
        pub mlme_init_sender: oneshot::Sender<Result<(), zx::Status>>,
        pub complete_mlme_sender: oneshot::Sender<Result<(), anyhow::Error>>,
        pub complete_sme_sender: oneshot::Sender<Result<(), anyhow::Error>>,
    }

    impl ServeTestHarness {
        fn new() -> (Pin<Box<impl Future<Output = Result<(), zx::Status>>>>, ServeTestHarness) {
            let (init_complete_sender, init_complete_receiver) = oneshot::channel();
            let init_completer = InitCompleter::new(move |result| {
                init_complete_sender.send(result).unwrap();
            });
            let (mlme_init_sender, mlme_init_receiver) = oneshot::channel();
            let (complete_mlme_sender, complete_mlme_receiver) = oneshot::channel();
            let mlme_fut = Box::pin(async { complete_mlme_receiver.await.unwrap() });
            let (complete_sme_sender, complete_sme_receiver) = oneshot::channel();
            let sme_fut = Box::pin(async { complete_sme_receiver.await.unwrap() });

            (
                Box::pin(serve(init_completer, mlme_init_receiver, mlme_fut, sme_fut)),
                ServeTestHarness {
                    init_complete_receiver: Box::pin(init_complete_receiver),
                    mlme_init_sender,
                    complete_mlme_sender,
                    complete_sme_sender,
                },
            )
        }
    }

    #[test]
    fn serve_exits_with_error_if_mlme_init_sender_dropped() {
        let mut exec = fasync::TestExecutor::new();
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        drop(harness.mlme_init_sender);
        assert_variant!(
            exec.run_until_stalled(&mut serve_fut),
            Poll::Ready(Err(zx::Status::INTERNAL))
        );

        assert_variant!(
            exec.run_until_stalled(&mut harness.init_complete_receiver),
            Poll::Ready(Ok(Err(zx::Status::INTERNAL)))
        );
    }

    #[test]
    fn serve_exits_with_error_on_init_failure() {
        let mut exec = fasync::TestExecutor::new();
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        harness.mlme_init_sender.send(Err(zx::Status::IO_NOT_PRESENT)).unwrap();
        assert_variant!(
            exec.run_until_stalled(&mut serve_fut),
            Poll::Ready(Err(zx::Status::IO_NOT_PRESENT))
        );
        assert_variant!(
            exec.run_until_stalled(&mut harness.init_complete_receiver),
            Poll::Ready(Ok(Err(zx::Status::IO_NOT_PRESENT)))
        );
    }

    #[test_case(Ok(()))]
    #[test_case(Err(format_err!("")))]
    fn serve_exits_with_error_if_mlme_completes_before_init(early_mlme_result: Result<(), Error>) {
        let mut exec = fasync::TestExecutor::new();
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        harness.complete_mlme_sender.send(early_mlme_result).unwrap();
        assert_variant!(
            exec.run_until_stalled(&mut serve_fut),
            Poll::Ready(Err(zx::Status::INTERNAL))
        );
        assert_variant!(
            exec.run_until_stalled(&mut harness.init_complete_receiver),
            Poll::Ready(Ok(Err(zx::Status::INTERNAL)))
        );
    }

    #[test_case(Ok(()))]
    #[test_case(Err(format_err!("")))]
    fn serve_exits_with_error_if_sme_shuts_down_before_mlme(early_sme_result: Result<(), Error>) {
        let mut exec = fasync::TestExecutor::new();
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        harness.mlme_init_sender.send(Ok(())).unwrap();
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut harness.init_complete_receiver),
            Poll::Ready(Ok(Ok(())))
        );
        harness.complete_sme_sender.send(early_sme_result).unwrap();
        assert_variant!(
            exec.run_until_stalled(&mut serve_fut),
            Poll::Ready(Err(zx::Status::INTERNAL))
        );
    }

    #[test]
    fn serve_exits_with_error_if_mlme_completes_with_error() {
        let mut exec = fasync::TestExecutor::new();
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        harness.mlme_init_sender.send(Ok(())).unwrap();
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut harness.init_complete_receiver),
            Poll::Ready(Ok(Ok(())))
        );
        harness.complete_mlme_sender.send(Err(format_err!("mlme error"))).unwrap();
        assert_eq!(exec.run_until_stalled(&mut serve_fut), Poll::Ready(Err(zx::Status::INTERNAL)));
    }

    #[test]
    fn serve_exits_with_error_if_sme_shuts_down_with_error() {
        let mut exec = fasync::TestExecutor::new();
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        harness.mlme_init_sender.send(Ok(())).unwrap();
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut harness.init_complete_receiver),
            Poll::Ready(Ok(Ok(())))
        );
        harness.complete_mlme_sender.send(Ok(())).unwrap();
        harness.complete_sme_sender.send(Err(format_err!("sme error"))).unwrap();
        assert_eq!(exec.run_until_stalled(&mut serve_fut), Poll::Ready(Err(zx::Status::INTERNAL)));
    }

    #[test]
    fn serve_ends_with_graceful_shutdown() {
        let mut exec = fasync::TestExecutor::new();
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        harness.mlme_init_sender.send(Ok(())).unwrap();
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut harness.init_complete_receiver),
            Poll::Ready(Ok(Ok(())))
        );
        harness.complete_mlme_sender.send(Ok(())).unwrap();
        assert_eq!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
        harness.complete_sme_sender.send(Ok(())).unwrap();
        assert_eq!(exec.run_until_stalled(&mut serve_fut), Poll::Ready(Ok(())));
    }

    #[derive(Debug)]
    struct StartAndServeTestHarness<F> {
        pub start_and_serve_fut: F,
        pub softmac_handle_receiver: oneshot::Receiver<Result<WlanSoftmacHandle, zx::Status>>,
        pub generic_sme_proxy: fidl_sme::GenericSmeProxy,
    }

    /// This function wraps start_and_serve() with a FakeDevice provided by a test.
    ///
    /// A returned Ok value will contain a tuple with the following values if start_and_serve()
    /// successfully bootstraps SME:
    ///
    ///   - start_and_serve() future.
    ///   - oneshot::Receiver to receive a WlanSoftmacHandle or an error.
    ///   - GenericSmeProxy
    ///
    /// The returned start_and_serve() future will run the WlanSoftmacIfcBridge, MLME, and SME servers when
    /// run on an executor.
    ///
    /// An Err value will be returned if start_and_serve() encounters an error completing the bootstrap
    /// of the SME server.
    fn start_and_serve_with_device(
        exec: &mut fasync::TestExecutor,
        fake_device: FakeDevice,
    ) -> Result<StartAndServeTestHarness<impl Future<Output = Result<(), zx::Status>>>, zx::Status>
    {
        let fake_buf_provider = wlan_mlme::buffer::FakeBufferProvider::new();
        let (softmac_handle_sender, mut softmac_handle_receiver) =
            oneshot::channel::<Result<WlanSoftmacHandle, zx::Status>>();
        let start_and_serve_fut = start_and_serve(
            move |result: Result<WlanSoftmacHandle, zx::Status>| {
                softmac_handle_sender
                    .send(result)
                    .expect("Failed to signal initialization complete.")
            },
            fake_device.clone(),
            fake_buf_provider,
        );
        let mut start_and_serve_fut = Box::pin(start_and_serve_fut);

        let usme_bootstrap_client_end = fake_device.state().lock().usme_bootstrap_client_end.take();
        match usme_bootstrap_client_end {
            // Simulate an errant initialization case where the UsmeBootstrap client end has been dropped
            // during initialization.
            None => match exec.run_until_stalled(&mut start_and_serve_fut) {
                Poll::Pending => panic!(
                    "start_and_serve() failed to panic when the UsmeBootstrap client was dropped."
                ),
                Poll::Ready(result) => {
                    // Assert the same initialization error appears in the receiver too.
                    let status = result.unwrap_err();
                    assert_eq!(
                        status,
                        assert_variant!(
                            exec.run_until_stalled(&mut softmac_handle_receiver),
                            Poll::Ready(Ok(Err(status))) => status
                        )
                    );
                    return Err(status);
                }
            },
            // Simulate the normal initialization case where the the UsmeBootstrap client end is active
            // during initialization.
            Some(usme_bootstrap_client_end) => {
                let (generic_sme_proxy, inspect_vmo_fut) =
                    bootstrap_generic_sme_proxy_and_inspect_vmo(usme_bootstrap_client_end);
                let start_and_serve_fut = match exec.run_until_stalled(&mut start_and_serve_fut) {
                    Poll::Pending => start_and_serve_fut,
                    Poll::Ready(result) => {
                        // Assert the same initialization error appears in the receiver too.
                        let status = result.unwrap_err();
                        assert_eq!(
                            status,
                            assert_variant!(
                                exec.run_until_stalled(&mut softmac_handle_receiver),
                                Poll::Ready(Ok(Err(status))) => status
                            )
                        );
                        return Err(status);
                    }
                };

                let inspect_vmo = exec.run_singlethreaded(inspect_vmo_fut);
                inspect_vmo.expect("Failed to bootstrap USME.");

                Ok(StartAndServeTestHarness {
                    start_and_serve_fut,
                    softmac_handle_receiver,
                    generic_sme_proxy,
                })
            }
        }
    }

    #[test]
    fn start_and_serve_fails_on_dropped_usme_bootstrap_client() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device_state.lock().usme_bootstrap_client_end = None;
        match start_and_serve_with_device(&mut exec, fake_device.clone()) {
            Ok(_) => panic!(
                "start_and_serve() does not fail when the UsmeBootstrap client end is dropped."
            ),
            Err(status) => assert_eq!(status, zx::Status::INTERNAL),
        }
    }

    // Exhaustive feature tests are unit tested on start()
    #[test]
    fn start_and_serve_fails_with_wrong_mac_implementation_type() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, _fake_device_state) = FakeDevice::new_with_config(
            &exec,
            FakeDeviceConfig::default()
                .with_mock_mac_implementation_type(fidl_common::MacImplementationType::Fullmac),
        );

        match start_and_serve_with_device(&mut exec, fake_device) {
            Ok(_) => panic!(
                "start_and_serve() future did not terminate before attempting bootstrap SME."
            ),
            Err(status) => assert_eq!(status, zx::Status::INTERNAL),
        };
    }

    #[test]
    fn start_and_serve_fails_on_dropped_mlme_event_stream() {
        let mut exec = fasync::TestExecutor::new();
        let (mut fake_device, _fake_device_state) = FakeDevice::new(&exec);
        let _ = fake_device.take_mlme_event_stream();
        match start_and_serve_with_device(&mut exec, fake_device.clone()) {
            Ok(_) => {
                panic!("start_and_serve() does not fail when the MLME event stream is missing.")
            }
            Err(status) => assert_eq!(status, zx::Status::INTERNAL),
        }
    }

    #[test]
    fn start_and_serve_fails_on_dropped_generic_sme_client() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, _fake_device_state) = FakeDevice::new(&exec);
        let StartAndServeTestHarness {
            mut start_and_serve_fut,
            mut softmac_handle_receiver,
            generic_sme_proxy,
        } = start_and_serve_with_device(&mut exec, fake_device)
            .expect("Failed to initiate wlansoftmac setup.");
        let _handle = assert_variant!(exec.run_until_stalled(&mut softmac_handle_receiver), Poll::Ready(Ok(Ok(handle))) => handle);
        assert_eq!(exec.run_until_stalled(&mut start_and_serve_fut), Poll::Pending);

        drop(generic_sme_proxy);

        assert_eq!(
            exec.run_until_stalled(&mut start_and_serve_fut),
            Poll::Ready(Err(zx::Status::INTERNAL))
        );
    }

    #[test]
    fn start_and_serve_responds_to_generic_sme_requests() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, _fake_device_state) = FakeDevice::new(&exec);
        let StartAndServeTestHarness {
            mut start_and_serve_fut,
            mut softmac_handle_receiver,
            generic_sme_proxy,
        } = start_and_serve_with_device(&mut exec, fake_device)
            .expect("Failed to initiate wlansoftmac setup.");
        let _handle = assert_variant!(exec.run_until_stalled(&mut softmac_handle_receiver), Poll::Ready(Ok(Ok(handle))) => handle);

        let (_sme_telemetry_proxy, sme_telemetry_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");
        let (_client_sme_proxy, client_sme_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");

        let resp_fut = generic_sme_proxy.get_sme_telemetry(sme_telemetry_server);
        pin_mut!(resp_fut);

        // First poll `get_sme_telemetry` to send a `GetSmeTelemetry` request to the SME server, and then
        // poll the SME server process it. Finally, expect `get_sme_telemetry` to complete with `Ok(())`.
        assert_variant!(exec.run_until_stalled(&mut resp_fut), Poll::Pending);
        assert_eq!(exec.run_until_stalled(&mut start_and_serve_fut), Poll::Pending);
        assert_variant!(exec.run_until_stalled(&mut resp_fut), Poll::Ready(Ok(Ok(()))));

        let resp_fut = generic_sme_proxy.get_client_sme(client_sme_server);
        pin_mut!(resp_fut);

        // First poll `get_client_sme` to send a `GetClientSme` request to the SME server, and then poll the
        // SME server process it. Finally, expect `get_client_sme` to complete with `Ok(())`.
        assert_variant!(exec.run_until_stalled(&mut resp_fut), Poll::Pending);
        assert_eq!(exec.run_until_stalled(&mut start_and_serve_fut), Poll::Pending);
        exec.run_singlethreaded(resp_fut)
            .expect("Generic SME proxy failed")
            .expect("Client SME request failed");
    }

    #[test]
    fn start_and_serve_shuts_down_gracefully() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, _fake_device_state) = FakeDevice::new(&exec);
        let StartAndServeTestHarness {
            mut start_and_serve_fut,
            mut softmac_handle_receiver,
            generic_sme_proxy,
        } = start_and_serve_with_device(&mut exec, fake_device)
            .expect("Failed to initiate wlansoftmac setup.");
        let (sme_telemetry_proxy, sme_telemetry_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");
        let (client_sme_proxy, client_sme_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");
        assert_eq!(exec.run_until_stalled(&mut start_and_serve_fut), Poll::Pending);
        let handle = assert_variant!(exec.run_until_stalled(&mut softmac_handle_receiver), Poll::Ready(Ok(Ok(handle))) => handle);

        let resp_fut = generic_sme_proxy.get_sme_telemetry(sme_telemetry_server);
        pin_mut!(resp_fut);

        // First poll `get_sme_telemetry` to send a `GetSmeTelemetry` request to the SME server, and then
        // poll the SME server process it. Finally, expect `get_sme_telemetry` to complete with `Ok(())`.
        assert_variant!(exec.run_until_stalled(&mut resp_fut), Poll::Pending);
        assert_eq!(exec.run_until_stalled(&mut start_and_serve_fut), Poll::Pending);
        assert_variant!(exec.run_until_stalled(&mut resp_fut), Poll::Ready(Ok(Ok(()))));

        let resp_fut = generic_sme_proxy.get_client_sme(client_sme_server);
        pin_mut!(resp_fut);

        // First poll `get_client_sme` to send a `GetClientSme` request to the SME server, and then poll the
        // SME server process it. Finally, expect `get_client_sme` to complete with `Ok(())`.
        assert_variant!(exec.run_until_stalled(&mut resp_fut), Poll::Pending);
        assert_eq!(exec.run_until_stalled(&mut start_and_serve_fut), Poll::Pending);
        exec.run_singlethreaded(resp_fut)
            .expect("Generic SME proxy failed")
            .expect("Client SME request failed");

        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        handle.stop(StopCompleter::new(Box::new(move || {
            shutdown_sender.send(()).expect("Failed to signal shutdown completion.")
        })));
        assert_variant!(
            exec.run_singlethreaded(async {
                futures::join!(start_and_serve_fut, shutdown_receiver)
            }),
            (Ok(()), Ok(()))
        );

        // All SME proxies should shutdown.
        assert!(generic_sme_proxy.is_closed());
        assert!(sme_telemetry_proxy.is_closed());
        assert!(client_sme_proxy.is_closed());
    }
}
