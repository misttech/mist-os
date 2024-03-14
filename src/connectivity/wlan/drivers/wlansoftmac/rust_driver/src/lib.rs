// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Error},
    fidl::endpoints::{ProtocolMarker, Proxy},
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_sme as fidl_sme,
    fidl_fuchsia_wlan_softmac as fidl_softmac,
    fuchsia_async::{Duration, Task},
    fuchsia_inspect::{Inspector, Node as InspectNode},
    fuchsia_inspect_contrib::auto_persist,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{
        channel::{
            mpsc,
            oneshot::{self, Canceled},
        },
        Future, FutureExt, StreamExt,
    },
    std::pin::Pin,
    tracing::{error, info, warn},
    wlan_fidl_ext::{ResponderExt, SendResultExt, WithName},
    wlan_mlme::{
        buffer::CBufferProvider,
        device::{
            completers::{InitCompleter, StopCompleter},
            DeviceOps,
        },
        DriverEvent, DriverEventSink,
    },
    wlan_sme::serve::create_sme,
    wlan_trace as wtrace,
};

const INSPECT_VMO_SIZE_BYTES: usize = 1000 * 1024;

pub struct WlanSoftmacHandle(DriverEventSink);

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
}

/// Run the Rust portion of wlansoftmac which includes the following three futures:
///
///   - WlanSoftmacIfcBridge server
///   - MLME server
///   - SME server
///
/// The WlanSoftmacIfcBridge server future executes on a parallel thread because otherwise
/// synchronous calls from the MLME server into the vendor driver could deadlock if the vendor
/// driver calls a WlanSoftmacIfcBridge method before returning from a synchronous call. For
/// example, when the MLME server synchronously calls WlanSoftmac.StartActiveScan(), the vendor
/// driver may call WlanSoftmacIfc.NotifyScanComplete() before returning from
/// WlanSoftmac.StartActiveScan(). This can occur when the scan request results in immediate
/// cancellation despite the request having valid arguments.
///
/// This function calls init_completer() when either MLME initialization completes successfully or
/// an error occurs before MLME initialization completes. The Ok() value passed by init_completer()
/// is a WlanSoftmacHandle which contains the FFI for calling WlanSoftmacIfc methods from the C++
/// portion of wlansoftmac.
///
/// The return value of this function is distinct from the value passed in init_completer(). This
/// function returns in one of four cases:
///
///   - An error occurred during initialization.
///   - An error occurred while running.
///   - An error occurred during shutdown.
///   - Shutdown completed successfully.
///
/// Generally, errors during initializations will be returned immediately after this function calls
/// init_completer() with the same error. Later errors can be returned after this functoin calls
/// init_completer().
pub async fn start_and_serve<D: DeviceOps + Send + 'static>(
    init_completer: impl FnOnce(Result<WlanSoftmacHandle, zx::Status>) + Send + 'static,
    device: D,
    buffer_provider: CBufferProvider,
) -> Result<(), zx::Status> {
    wtrace::duration_begin_scope!(c"rust_driver::start_and_serve");
    let (driver_event_sink, driver_event_stream) = DriverEventSink::new();
    let softmac_handle_sink = driver_event_sink.clone();
    let init_completer = InitCompleter::new(move |result: Result<(), zx::Status>| {
        init_completer(result.map(|()| WlanSoftmacHandle(softmac_handle_sink)))
    });

    let (mlme_init_sender, mlme_init_receiver) = oneshot::channel();
    let StartedDriver { softmac_ifc_bridge_request_stream, mlme, sme } = match start(
        mlme_init_sender,
        driver_event_sink.clone(),
        driver_event_stream,
        device,
        buffer_provider,
    )
    .await
    {
        Err(status) => {
            init_completer.complete(Err(status));
            return Err(status);
        }
        Ok(x) => x,
    };

    serve(
        init_completer,
        mlme_init_receiver,
        driver_event_sink,
        softmac_ifc_bridge_request_stream,
        mlme,
        sme,
    )
    .await
}

struct StartedDriver<Mlme, Sme> {
    pub softmac_ifc_bridge_request_stream: fidl_softmac::WlanSoftmacIfcBridgeRequestStream,
    pub mlme: Mlme,
    pub sme: Sme,
}

/// Start the bridged wlansoftmac driver by creating components to run two futures:
///
///   - MLME server
///   - SME server
///
/// This function will use the provided |device| to make various calls into the vendor driver
/// necessary to configure and create the components to run the futures.
async fn start<D: DeviceOps + Send + 'static>(
    mlme_init_sender: oneshot::Sender<Result<(), zx::Status>>,
    driver_event_sink: DriverEventSink,
    driver_event_stream: mpsc::UnboundedReceiver<DriverEvent>,
    mut device: D,
    buffer_provider: CBufferProvider,
) -> Result<
    StartedDriver<
        Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>,
        Pin<Box<impl Future<Output = Result<(), Error>>>>,
    >,
    zx::Status,
> {
    wtrace::duration!(c"rust_driver::start");

    let (softmac_ifc_bridge_proxy, softmac_ifc_bridge_request_stream) =
        fidl::endpoints::create_proxy_and_stream::<fidl_softmac::WlanSoftmacIfcBridgeMarker>()
            .map_err(|e| {
                // Failure to unwrap indicates a critical failure in the driver init thread.
                error!(
                    "Failed to get {} server stream: {}",
                    fidl_softmac::WlanSoftmacIfcBridgeMarker::DEBUG_NAME,
                    e
                );
                zx::Status::INTERNAL
            })?;

    // Bootstrap USME
    let BootstrappedGenericSme { generic_sme_request_stream, legacy_privacy_support, inspect_node } =
        bootstrap_generic_sme(&mut device, driver_event_sink, softmac_ifc_bridge_proxy).await?;

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

    // TODO(https://fxbug.dev/42077094): The MLME event stream should be moved out of DeviceOps
    // entirely.
    let mlme_event_stream = match device.take_mlme_event_stream() {
        Some(mlme_event_stream) => mlme_event_stream,
        None => {
            error!("Failed to take MLME event stream.");
            return Err(zx::Status::INTERNAL);
        }
    };

    // Create an SME future to serve
    let (mlme_request_stream, sme) = match create_sme(
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
        Ok((mlme_request_stream, sme)) => (mlme_request_stream, sme),
        Err(e) => {
            error!("Failed to create sme: {}", e);
            return Err(zx::Status::INTERNAL);
        }
    };

    // Create an MLME future to serve
    let mlme: Pin<Box<dyn Future<Output = Result<(), Error>> + Send>> = match device_info.role {
        fidl_common::WlanMacRole::Client => {
            info!("Running wlansoftmac with client role");
            let config = wlan_mlme::client::ClientConfig {
                ensure_on_channel_time: Duration::from_millis(500).into_nanos(),
            };
            Box::pin(wlan_mlme::mlme_main_loop::<wlan_mlme::client::ClientMlme<D>>(
                mlme_init_sender,
                config,
                device,
                buffer_provider,
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
                buffer_provider,
                mlme_request_stream,
                driver_event_stream,
            ))
        }
        unsupported => {
            error!("Unsupported mac role: {:?}", unsupported);
            return Err(zx::Status::INTERNAL);
        }
    };

    Ok(StartedDriver { softmac_ifc_bridge_request_stream, mlme, sme })
}

/// Await on futures hosting the following three servers:
///
///   - WlanSoftmacIfcBridge server
///   - MLME server
///   - SME server
///
/// The WlanSoftmacIfcBridge server runs on a parallel thread but will be shut down before this
/// function returns. This is true even if this function exits with an error.
///
/// Upon receiving a DriverEvent::Stop, the MLME server will shut down first. Then this function
/// will await the completion of WlanSoftmacIfcBridge server and SME server. Both will shut down as
/// a consequence of MLME server shut down.
async fn serve<F>(
    init_completer: InitCompleter<F>,
    mlme_init_receiver: oneshot::Receiver<Result<(), zx::Status>>,
    driver_event_sink: DriverEventSink,
    softmac_ifc_bridge_request_stream: fidl_softmac::WlanSoftmacIfcBridgeRequestStream,
    mlme: Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>,
    sme: Pin<Box<impl Future<Output = Result<(), Error>>>>,
) -> Result<(), zx::Status>
where
    F: FnOnce(Result<(), zx::Status>) + Send,
{
    wtrace::duration_begin_scope!(c"rust_driver::serve");

    // Create a oneshot::channel to signal to this executor when WlanSoftmacIfcBridge
    // server exits.
    let (bridge_exit_sender, bridge_exit_receiver) = oneshot::channel();
    // Spawn a Task to host the WlanSoftmacIfcBridge server.
    let bridge = Task::spawn(async move {
        let _: Result<(), ()> = bridge_exit_sender
            .send(
                serve_wlan_softmac_ifc_bridge(driver_event_sink, softmac_ifc_bridge_request_stream)
                    .await,
            )
            .map_err(|result| {
                error!("Failed to send serve_wlan_softmac_ifc_bridge() result: {:?}", result)
            });
    });

    let mut mlme = mlme.fuse();
    let mut sme = sme.fuse();

    // oneshot::Receiver implements FusedFuture incorrectly, so we must call .fuse()
    // to get the right behavior in the select!().
    //
    // See https://github.com/rust-lang/futures-rs/issues/2455 for more details.
    let mut bridge_exit_receiver = bridge_exit_receiver.fuse();
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
                    bridge.cancel().await;
                        init_completer.complete(Err(status));
                        return Err(status);
                    }
                };
                match init_result {
                    Ok(()) =>
                        init_completer.complete(Ok(())),
                    Err(status) => {
                        error!("Failed to initialize MLME: {}", status);
                    bridge.cancel().await;
                        init_completer.complete(Err(status));
                        return Err(status);
                    }
                }
            },
            mlme_result = mlme => {
                error!("MLME future completed before signaling init_sender: {:?}", mlme_result);
                let status = zx::Status::INTERNAL;
                bridge.cancel().await;
                init_completer.complete(Err(status));
                return Err(status);
            }
        }
    }

    // Run the SME and MLME servers.
    let server_shutdown_result = {
        wtrace::duration_begin_scope!(c"run MLME and SME");
        futures::select! {
        mlme_result = mlme => {
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
        bridge_result = bridge_exit_receiver => {
            error!("SoftmacIfcBridge server shut down before MLME: {:?}", bridge_result);
            Err(zx::Status::INTERNAL)
        }
        sme_result = sme => {
            error!("SME shut down before MLME: {:?}", sme_result);
            Err(zx::Status::INTERNAL)
        }}
    };

    // If any future returns an error, return with the same error without waiting for other futures
    // to complete.
    server_shutdown_result?;

    // At this point, the `bridge` Task should not report a result because the WlanSoftmacIfcBridge
    // server is still running. This cancellation should cause `bridge_exit_receiver` to be dropped.
    bridge
        .cancel()
        .await
        .map(|()| warn!("SoftmacIfcBridge server task completed before cancelation."));
    let bridge_result: Result<(), ()> = match bridge_exit_receiver.await {
        Err(Canceled) => {
            // This is the expected case because when MLME shuts down, the SoftmacIfcBridge server
            // task should still be running.
            info!("SoftmacIfcBridge server shut down gracefully");
            Ok(())
        }
        Ok(result) => {
            warn!(
                "SoftmacIfcBridge server task completed without canceling bridge_exit_receiver.\n\
                   This indicates the server was already shut down before its task was canceled."
            );
            result.map_err(|e| error!("SoftmacIfcBridge server shut down with error: {}", e))
        }
    };

    // Since the MLME server is shut down at this point, the SME server will shut down soon because
    // the SME server always shuts down when it loses connection with the MLME server.
    let sme_result: Result<(), ()> = sme
        .await
        .map(|()| info!("SME shut down gracefully"))
        .map_err(|e| error!("SME shut down with error: {}", e));

    bridge_result.and(sme_result).map_err(|()| zx::Status::INTERNAL)
}

struct BootstrappedGenericSme {
    pub generic_sme_request_stream: fidl_sme::GenericSmeRequestStream,
    pub legacy_privacy_support: fidl_sme::LegacyPrivacySupport,
    pub inspect_node: InspectNode,
}

/// Call WlanSoftmac.Start() to retrieve the server end of UsmeBootstrap channel and wait
/// for a UsmeBootstrap.Start() message to provide the server end of a GenericSme channel.
///
/// Any errors encountered in this function are fatal for the wlansoftmac driver. Failure to
/// bootstrap GenericSme request stream will result in a driver no other component can communicate
/// with.
async fn bootstrap_generic_sme<D: DeviceOps>(
    device: &mut D,
    driver_event_sink: DriverEventSink,
    softmac_ifc_bridge_proxy: fidl_softmac::WlanSoftmacIfcBridgeProxy,
) -> Result<BootstrappedGenericSme, zx::Status> {
    wtrace::duration!(c"rust_driver::bootstrap_generic_sme");

    let wlan_softmac_ifc_bridge_client_handle = zx::Handle::from(
        softmac_ifc_bridge_proxy
            .into_channel()
            .map_err(|_| {
                error!(
                    "Failed to convert {} into channel.",
                    fidl_softmac::WlanSoftmacIfcBridgeMarker::DEBUG_NAME
                );
                zx::Status::INTERNAL
            })?
            .into_zx_channel(),
    )
    .into_raw();

    // Calling WlanSoftmac.Start() indicates to the vendor driver that this driver (wlansoftmac) is
    // ready to receive WlanSoftmacIfc messages. wlansoftmac will buffer all WlanSoftmacIfc messages
    // in an mpsc::UnboundedReceiver<DriverEvent> sink until the MLME server drains them.
    let usme_bootstrap_handle_via_iface_creation =
        match device.start(driver_event_sink, wlan_softmac_ifc_bridge_client_handle) {
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

async fn serve_wlan_softmac_ifc_bridge(
    driver_event_sink: DriverEventSink,
    mut softmac_ifc_bridge_request_stream: fidl_softmac::WlanSoftmacIfcBridgeRequestStream,
) -> Result<(), anyhow::Error> {
    loop {
        let request = match softmac_ifc_bridge_request_stream.next().await {
            Some(Ok(request)) => request,
            Some(Err(e)) => {
                return Err(format_err!("WlanSoftmacIfcBridge server stream failed: {}", e));
            }
            None => {
                info!("WlanSoftmacIfcBridge stream terminated");
                return Ok(());
            }
        };
        match request {
            fidl_softmac::WlanSoftmacIfcBridgeRequest::ReportTxResult { tx_result, responder } => {
                let responder = driver_event_sink.unbounded_send_or_respond(
                    DriverEvent::TxResultReport { tx_result },
                    responder,
                    (),
                )?;
                responder.send().format_send_err_with_context("ReportTxResult")?;
            }
            fidl_softmac::WlanSoftmacIfcBridgeRequest::NotifyScanComplete {
                payload,
                responder,
            } => {
                let ((status, scan_id), responder) = responder.unpack_fields_or_respond((
                    payload.status.with_name("status"),
                    payload.scan_id.with_name("scan_id"),
                ))?;
                let status = zx::Status::from_raw(status);
                let responder = driver_event_sink.unbounded_send_or_respond(
                    DriverEvent::ScanComplete { status, scan_id },
                    responder,
                    (),
                )?;
                responder.send().format_send_err_with_context("NotifyScanComplete")?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        anyhow::format_err,
        diagnostics_assertions::assert_data_tree,
        fuchsia_async::TestExecutor,
        fuchsia_inspect::InspectorConfig,
        futures::{stream::FuturesUnordered, task::Poll},
        pin_utils::pin_mut,
        std::sync::Arc,
        test_case::test_case,
        wlan_common::assert_variant,
        wlan_mlme::device::test_utils::{FakeDevice, FakeDeviceConfig},
        zx::Vmo,
    };

    struct BootstrapGenericSmeTestHarness {
        _softmac_ifc_bridge_request_stream: fidl_softmac::WlanSoftmacIfcBridgeRequestStream,
    }

    // We could implement BootstrapGenericSmeTestHarness::new() instead of a macro, but doing so requires
    // pinning the FakeDevice and WlanSoftmacIfcProtocol (and its associated DriverEventSink). While the
    // pinning itself is feasible, it leads to a complex harness implementation that outweighs the benefit
    // of using a harness to begin with.
    macro_rules! make_bootstrap_generic_sme_test_harness {
        (&mut $fake_device:ident, $driver_event_sink:ident $(,)?) => {{
            let (softmac_ifc_bridge_proxy, _softmac_ifc_bridge_request_stream) =
                fidl::endpoints::create_proxy_and_stream::<fidl_softmac::WlanSoftmacIfcBridgeMarker>().unwrap();
            (
                Box::pin(bootstrap_generic_sme(
                    &mut $fake_device,
                    $driver_event_sink,
                    softmac_ifc_bridge_proxy,
                )),
                BootstrapGenericSmeTestHarness {
                    _softmac_ifc_bridge_request_stream,
                }
            )
        }};
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn bootstrap_generic_sme_fails_to_retrieve_usme_bootstrap_handle() {
        let (mut fake_device, _fake_device_state) = FakeDevice::new_with_config(
            FakeDeviceConfig::default().with_mock_start_result(Err(zx::Status::INTERRUPTED_RETRY)),
        )
        .await;
        let (driver_event_sink, _driver_event_stream) = DriverEventSink::new();

        let (mut bootstrap_generic_sme_fut, _harness) =
            make_bootstrap_generic_sme_test_harness!(&mut fake_device, driver_event_sink);
        match TestExecutor::poll_until_stalled(&mut bootstrap_generic_sme_fut).await {
            Poll::Ready(Err(zx::Status::INTERRUPTED_RETRY)) => (),
            Poll::Ready(Err(status)) => panic!("Failed with wrong status: {}", status),
            Poll::Ready(Ok(_)) => panic!("Succeeded unexpectedly"),
            Poll::Pending => panic!("bootstrap_generic_sme() unexpectedly stalled"),
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn boostrap_generic_sme_fails_on_error_from_bootstrap_stream() {
        let (mut fake_device, fake_device_state) =
            FakeDevice::new_with_config(FakeDeviceConfig::default()).await;
        let (driver_event_sink, _driver_event_stream) = DriverEventSink::new();

        let (mut bootstrap_generic_sme_fut, _harness) =
            make_bootstrap_generic_sme_test_harness!(&mut fake_device, driver_event_sink);
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut bootstrap_generic_sme_fut).await,
            Poll::Pending
        ));

        // Write an invalid FIDL message to the USME bootstrap channel.
        let usme_bootstrap_channel =
            fake_device_state.lock().usme_bootstrap_client_end.take().unwrap().into_channel();
        usme_bootstrap_channel.write(&[], &mut []).unwrap();

        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut bootstrap_generic_sme_fut).await,
            Poll::Ready(Err(zx::Status::INTERNAL))
        ));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn boostrap_generic_sme_fails_on_closed_bootstrap_stream() {
        let (mut fake_device, fake_device_state) =
            FakeDevice::new_with_config(FakeDeviceConfig::default()).await;
        let (driver_event_sink, _driver_event_stream) = DriverEventSink::new();

        let (mut bootstrap_generic_sme_fut, _harness) =
            make_bootstrap_generic_sme_test_harness!(&mut fake_device, driver_event_sink);
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut bootstrap_generic_sme_fut).await,
            Poll::Pending
        ));

        // Drop the client end of USME bootstrap channel.
        let _ = fake_device_state.lock().usme_bootstrap_client_end.take().unwrap();

        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut bootstrap_generic_sme_fut).await,
            Poll::Ready(Err(zx::Status::INTERNAL))
        ));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn boostrap_generic_sme_succeeds() {
        let (mut fake_device, fake_device_state) =
            FakeDevice::new_with_config(FakeDeviceConfig::default()).await;
        let (driver_event_sink, _driver_event_stream) = DriverEventSink::new();

        let (mut bootstrap_generic_sme_fut, _harness) =
            make_bootstrap_generic_sme_test_harness!(&mut fake_device, driver_event_sink);
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut bootstrap_generic_sme_fut).await,
            Poll::Pending
        ));

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
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut inspect_vmo_fut).await,
            Poll::Pending
        ));

        let BootstrappedGenericSme {
            mut generic_sme_request_stream,
            legacy_privacy_support: received_legacy_privacy_support,
            inspect_node,
        } = match TestExecutor::poll_until_stalled(&mut bootstrap_generic_sme_fut).await {
            Poll::Pending => panic!("bootstrap_generic_sme_fut() did not complete!"),
            Poll::Ready(x) => x.unwrap(),
        };
        let inspect_vmo = match TestExecutor::poll_until_stalled(&mut inspect_vmo_fut).await {
            Poll::Pending => panic!("Failed to receive an inspect VMO."),
            Poll::Ready(x) => x.unwrap(),
        };

        // Send a GenericSme.Query() to check the generic_sme_proxy
        // and generic_sme_stream are connected.
        let query_fut = generic_sme_proxy.query();
        pin_mut!(query_fut);
        assert!(matches!(TestExecutor::poll_until_stalled(&mut query_fut).await, Poll::Pending));
        let next_generic_sme_request_fut = generic_sme_request_stream.next();
        pin_mut!(next_generic_sme_request_fut);
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut next_generic_sme_request_fut).await,
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
        pub driver_event_sink: DriverEventSink,
    }

    impl StartTestHarness {
        fn new(
            fake_device: FakeDevice,
        ) -> (
            impl Future<
                Output = Result<
                    StartedDriver<
                        Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>,
                        Pin<Box<impl Future<Output = Result<(), Error>>>>,
                    >,
                    zx::Status,
                >,
            >,
            Self,
        ) {
            let (mlme_init_sender, mlme_init_receiver) = oneshot::channel();
            let (driver_event_sink, driver_event_stream) = DriverEventSink::new();
            let fake_buffer_provider = wlan_mlme::buffer::FakeCBufferProvider::new();

            (
                Box::pin(start(
                    mlme_init_sender,
                    driver_event_sink.clone(),
                    driver_event_stream,
                    fake_device,
                    fake_buffer_provider,
                )),
                Self { mlme_init_receiver: Box::pin(mlme_init_receiver), driver_event_sink },
            )
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_fails_on_bad_bootstrap() {
        let (fake_device, _fake_device_state) = FakeDevice::new_with_config(
            FakeDeviceConfig::default().with_mock_start_result(Err(zx::Status::INTERRUPTED_RETRY)),
        )
        .await;
        let (mut start_fut, _harness) = StartTestHarness::new(fake_device);

        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut start_fut).await,
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
    #[fuchsia::test(allow_stalls = false)]
    async fn start_fails_on_query_error(
        fake_device_config: FakeDeviceConfig,
        expected_status: zx::Status,
    ) {
        let (fake_device, fake_device_state) =
            FakeDevice::new_with_config(fake_device_config).await;
        let (mut start_fut, _harness) = StartTestHarness::new(fake_device);

        let usme_bootstrap_client_end =
            fake_device_state.lock().usme_bootstrap_client_end.take().unwrap();
        let (_generic_sme_proxy, _inspect_vmo_fut) =
            bootstrap_generic_sme_proxy_and_inspect_vmo(usme_bootstrap_client_end);

        match TestExecutor::poll_until_stalled(&mut start_fut).await {
            Poll::Ready(Err(status)) => assert_eq!(status, expected_status),
            Poll::Pending => panic!("start_fut still pending!"),
            Poll::Ready(Ok(_)) => panic!("start_fut completed with Ok value"),
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_fail_on_dropped_mlme_event_stream() {
        let (fake_device, fake_device_state) = FakeDevice::new().await;
        let (mut start_fut, _harness) = StartTestHarness::new(fake_device);

        let usme_bootstrap_client_end =
            fake_device_state.lock().usme_bootstrap_client_end.take().unwrap();
        let (_generic_sme_proxy, _inspect_vmo_fut) =
            bootstrap_generic_sme_proxy_and_inspect_vmo(usme_bootstrap_client_end);

        let _ = fake_device_state.lock().mlme_event_stream.take();
        match TestExecutor::poll_until_stalled(&mut start_fut).await {
            Poll::Ready(Err(status)) => assert_eq!(status, zx::Status::INTERNAL),
            Poll::Pending => panic!("start_fut still pending!"),
            Poll::Ready(Ok(_)) => panic!("start_fut completed with Ok value"),
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_succeeds() {
        let (fake_device, fake_device_state) = FakeDevice::new_with_config(
            FakeDeviceConfig::default()
                .with_mock_sta_addr([2u8; 6])
                .with_mock_mac_role(fidl_common::WlanMacRole::Client),
        )
        .await;
        let (mut start_fut, mut harness) = StartTestHarness::new(fake_device);

        let usme_bootstrap_client_end =
            fake_device_state.lock().usme_bootstrap_client_end.take().unwrap();
        let (generic_sme_proxy, _inspect_vmo_fut) =
            bootstrap_generic_sme_proxy_and_inspect_vmo(usme_bootstrap_client_end);

        let StartedDriver {
            softmac_ifc_bridge_request_stream: _softmac_ifc_bridge_request_stream,
            mut mlme,
            sme,
        } = match TestExecutor::poll_until_stalled(&mut start_fut).await {
            Poll::Ready(Ok(x)) => x,
            Poll::Ready(Err(status)) => {
                panic!("start_fut unexpectedly failed; {}", status)
            }
            Poll::Pending => panic!("start_fut still pending!"),
        };

        assert_variant!(TestExecutor::poll_until_stalled(&mut mlme).await, Poll::Pending);
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut harness.mlme_init_receiver).await,
            Poll::Ready(Ok(Ok(())))
        ));

        let resp_fut = generic_sme_proxy.query();
        pin_mut!(resp_fut);
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Pending);

        let sme_and_mlme = [sme, mlme].into_iter().collect::<FuturesUnordered<_>>();
        pin_mut!(sme_and_mlme);
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut sme_and_mlme.next()).await,
            Poll::Pending
        ));

        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut resp_fut).await,
            Poll::Ready(Ok(fidl_sme::GenericSmeQuery {
                role: fidl_common::WlanMacRole::Client,
                sta_addr: [2, 2, 2, 2, 2, 2],
            }))
        ));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn serve_wlansoftmac_ifc_bridge_fails_on_request_stream_error() {
        let (driver_event_sink, _driver_event_stream) = DriverEventSink::new();
        let (softmac_ifc_bridge_client, softmac_ifc_bridge_server) =
            fidl::endpoints::create_endpoints::<fidl_softmac::WlanSoftmacIfcBridgeMarker>();
        let softmac_ifc_bridge_request_stream = softmac_ifc_bridge_server.into_stream().unwrap();
        let softmac_ifc_bridge_channel = softmac_ifc_bridge_client.into_channel();

        let server_fut =
            serve_wlan_softmac_ifc_bridge(driver_event_sink, softmac_ifc_bridge_request_stream);
        pin_mut!(server_fut);
        assert_variant!(TestExecutor::poll_until_stalled(&mut server_fut).await, Poll::Pending);

        softmac_ifc_bridge_channel.write(&[], &mut []).unwrap();
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut server_fut).await,
            Poll::Ready(Err(_))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn serve_wlansoftmac_ifc_bridge_exits_on_request_stream_end() {
        let (driver_event_sink, _driver_event_stream) = DriverEventSink::new();
        let (softmac_ifc_bridge_client, softmac_ifc_bridge_server) =
            fidl::endpoints::create_endpoints::<fidl_softmac::WlanSoftmacIfcBridgeMarker>();
        let softmac_ifc_bridge_request_stream = softmac_ifc_bridge_server.into_stream().unwrap();

        let server_fut =
            serve_wlan_softmac_ifc_bridge(driver_event_sink, softmac_ifc_bridge_request_stream);
        pin_mut!(server_fut);
        assert_variant!(TestExecutor::poll_until_stalled(&mut server_fut).await, Poll::Pending);

        drop(softmac_ifc_bridge_client);
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut server_fut).await,
            Poll::Ready(Ok(()))
        );
    }

    #[test_case(fidl_softmac::WlanSoftmacIfcBaseNotifyScanCompleteRequest {
                status: None,
                scan_id: Some(754),
                ..Default::default()
    })]
    #[test_case(fidl_softmac::WlanSoftmacIfcBaseNotifyScanCompleteRequest {
                status: Some(zx::Status::OK.into_raw()),
                scan_id: None,
                ..Default::default()
            })]
    #[fuchsia::test(allow_stalls = false)]
    async fn serve_wlansoftmac_ifc_bridge_exits_on_invalid_notify_scan_complete_request(
        request: fidl_softmac::WlanSoftmacIfcBaseNotifyScanCompleteRequest,
    ) {
        let (driver_event_sink, mut driver_event_stream) = DriverEventSink::new();
        let (softmac_ifc_bridge_proxy, softmac_ifc_bridge_server) =
            fidl::endpoints::create_proxy::<fidl_softmac::WlanSoftmacIfcBridgeMarker>().unwrap();
        let softmac_ifc_bridge_request_stream = softmac_ifc_bridge_server.into_stream().unwrap();

        let server_fut =
            serve_wlan_softmac_ifc_bridge(driver_event_sink, softmac_ifc_bridge_request_stream);
        pin_mut!(server_fut);

        let resp_fut = softmac_ifc_bridge_proxy.notify_scan_complete(&request);
        pin_mut!(resp_fut);
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Pending);
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut server_fut).await,
            Poll::Ready(Err(_))
        );
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Ready(Ok(())));
        assert!(matches!(driver_event_stream.try_next(), Ok(None)));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn serve_wlansoftmac_ifc_bridge_enqueues_notify_scan_complete() {
        let (driver_event_sink, mut driver_event_stream) = DriverEventSink::new();
        let (softmac_ifc_bridge_proxy, softmac_ifc_bridge_server) =
            fidl::endpoints::create_proxy::<fidl_softmac::WlanSoftmacIfcBridgeMarker>().unwrap();
        let softmac_ifc_bridge_request_stream = softmac_ifc_bridge_server.into_stream().unwrap();

        let server_fut =
            serve_wlan_softmac_ifc_bridge(driver_event_sink, softmac_ifc_bridge_request_stream);
        pin_mut!(server_fut);

        let resp_fut = softmac_ifc_bridge_proxy.notify_scan_complete(
            &fidl_softmac::WlanSoftmacIfcBaseNotifyScanCompleteRequest {
                status: Some(zx::Status::OK.into_raw()),
                scan_id: Some(754),
                ..Default::default()
            },
        );
        pin_mut!(resp_fut);
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Pending);
        assert_variant!(TestExecutor::poll_until_stalled(&mut server_fut).await, Poll::Pending);
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Ready(Ok(())));

        assert!(matches!(
            driver_event_stream.try_next(),
            Ok(Some(DriverEvent::ScanComplete { status: zx::Status::OK, scan_id: 754 }))
        ));
    }

    struct ServeTestHarness {
        pub init_complete_receiver: Pin<Box<oneshot::Receiver<Result<(), zx::Status>>>>,
        pub mlme_init_sender: oneshot::Sender<Result<(), zx::Status>>,
        #[allow(dead_code)]
        pub driver_event_stream: mpsc::UnboundedReceiver<DriverEvent>,
        #[allow(dead_code)]
        pub softmac_ifc_bridge_proxy: fidl_softmac::WlanSoftmacIfcBridgeProxy,
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
            let (driver_event_sink, driver_event_stream) = DriverEventSink::new();
            let (softmac_ifc_bridge_proxy, softmac_ifc_bridge_server) =
                fidl::endpoints::create_proxy::<fidl_softmac::WlanSoftmacIfcBridgeMarker>()
                    .unwrap();
            let softmac_ifc_bridge_request_stream =
                softmac_ifc_bridge_server.into_stream().unwrap();
            let (complete_mlme_sender, complete_mlme_receiver) = oneshot::channel();
            let mlme = Box::pin(async { complete_mlme_receiver.await.unwrap() });
            let (complete_sme_sender, complete_sme_receiver) = oneshot::channel();
            let sme = Box::pin(async { complete_sme_receiver.await.unwrap() });

            (
                Box::pin(serve(
                    init_completer,
                    mlme_init_receiver,
                    driver_event_sink,
                    softmac_ifc_bridge_request_stream,
                    mlme,
                    sme,
                )),
                ServeTestHarness {
                    init_complete_receiver: Box::pin(init_complete_receiver),
                    mlme_init_sender,
                    driver_event_stream,
                    softmac_ifc_bridge_proxy,
                    complete_mlme_sender,
                    complete_sme_sender,
                },
            )
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn serve_wlansoftmac_ifc_bridge_enqueues_report_tx_result() {
        let (driver_event_sink, mut driver_event_stream) = DriverEventSink::new();
        let (softmac_ifc_bridge_proxy, softmac_ifc_bridge_server) =
            fidl::endpoints::create_proxy::<fidl_softmac::WlanSoftmacIfcBridgeMarker>().unwrap();
        let softmac_ifc_bridge_request_stream = softmac_ifc_bridge_server.into_stream().unwrap();

        let server_fut =
            serve_wlan_softmac_ifc_bridge(driver_event_sink, softmac_ifc_bridge_request_stream);
        pin_mut!(server_fut);

        let resp_fut = softmac_ifc_bridge_proxy.report_tx_result(&fidl_common::WlanTxResult {
            tx_result_entry: [fidl_common::WlanTxResultEntry {
                tx_vector_idx: fidl_common::WLAN_TX_VECTOR_IDX_INVALID,
                attempts: 0,
            }; fidl_common::WLAN_TX_RESULT_MAX_ENTRY as usize],
            peer_addr: [3; 6],
            result_code: fidl_common::WlanTxResultCode::Failed,
        });
        pin_mut!(resp_fut);
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Pending);
        assert_variant!(TestExecutor::poll_until_stalled(&mut server_fut).await, Poll::Pending);
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Ready(Ok(())));

        match driver_event_stream.try_next().unwrap().unwrap() {
            DriverEvent::TxResultReport { tx_result } => {
                assert_eq!(
                    tx_result,
                    fidl_common::WlanTxResult {
                        tx_result_entry: [fidl_common::WlanTxResultEntry {
                            tx_vector_idx: fidl_common::WLAN_TX_VECTOR_IDX_INVALID,
                            attempts: 0
                        };
                            fidl_common::WLAN_TX_RESULT_MAX_ENTRY as usize],
                        peer_addr: [3; 6],
                        result_code: fidl_common::WlanTxResultCode::Failed,
                    }
                );
            }
            _ => panic!("Unexpected DriverEvent!"),
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn serve_exits_with_error_if_mlme_init_sender_dropped() {
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        drop(harness.mlme_init_sender);
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut serve_fut).await,
            Poll::Ready(Err(zx::Status::INTERNAL))
        );

        assert_variant!(
            TestExecutor::poll_until_stalled(&mut harness.init_complete_receiver).await,
            Poll::Ready(Ok(Err(zx::Status::INTERNAL)))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn serve_exits_with_error_on_init_failure() {
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        harness.mlme_init_sender.send(Err(zx::Status::IO_NOT_PRESENT)).unwrap();
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut serve_fut).await,
            Poll::Ready(Err(zx::Status::IO_NOT_PRESENT))
        );
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut harness.init_complete_receiver).await,
            Poll::Ready(Ok(Err(zx::Status::IO_NOT_PRESENT)))
        );
    }

    #[test_case(Ok(()))]
    #[test_case(Err(format_err!("")))]
    #[fuchsia::test(allow_stalls = false)]
    async fn serve_exits_with_error_if_mlme_completes_before_init(
        early_mlme_result: Result<(), Error>,
    ) {
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        harness.complete_mlme_sender.send(early_mlme_result).unwrap();
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut serve_fut).await,
            Poll::Ready(Err(zx::Status::INTERNAL))
        );
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut harness.init_complete_receiver).await,
            Poll::Ready(Ok(Err(zx::Status::INTERNAL)))
        );
    }

    #[test_case(Ok(()))]
    #[test_case(Err(format_err!("")))]
    #[fuchsia::test(allow_stalls = false)]
    async fn serve_exits_with_error_if_sme_shuts_down_before_mlme(
        early_sme_result: Result<(), Error>,
    ) {
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        harness.mlme_init_sender.send(Ok(())).unwrap();
        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut harness.init_complete_receiver).await,
            Poll::Ready(Ok(Ok(())))
        );
        harness.complete_sme_sender.send(early_sme_result).unwrap();
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut serve_fut).await,
            Poll::Ready(Err(zx::Status::INTERNAL))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn serve_exits_with_error_if_mlme_completes_with_error() {
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        harness.mlme_init_sender.send(Ok(())).unwrap();
        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut harness.init_complete_receiver).await,
            Poll::Ready(Ok(Ok(())))
        );
        harness.complete_mlme_sender.send(Err(format_err!("mlme error"))).unwrap();
        assert_eq!(
            TestExecutor::poll_until_stalled(&mut serve_fut).await,
            Poll::Ready(Err(zx::Status::INTERNAL))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn serve_exits_with_error_if_sme_shuts_down_with_error() {
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        harness.mlme_init_sender.send(Ok(())).unwrap();
        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut harness.init_complete_receiver).await,
            Poll::Ready(Ok(Ok(())))
        );
        harness.complete_mlme_sender.send(Ok(())).unwrap();
        harness.complete_sme_sender.send(Err(format_err!("sme error"))).unwrap();
        assert_eq!(
            TestExecutor::poll_until_stalled(&mut serve_fut).await,
            Poll::Ready(Err(zx::Status::INTERNAL))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn serve_shuts_down_gracefully() {
        let (mut serve_fut, mut harness) = ServeTestHarness::new();

        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        harness.mlme_init_sender.send(Ok(())).unwrap();
        assert_variant!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut harness.init_complete_receiver).await,
            Poll::Ready(Ok(Ok(())))
        );
        harness.complete_mlme_sender.send(Ok(())).unwrap();
        assert_eq!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Pending);
        harness.complete_sme_sender.send(Ok(())).unwrap();
        assert_eq!(TestExecutor::poll_until_stalled(&mut serve_fut).await, Poll::Ready(Ok(())));
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
    async fn start_and_serve_with_device(
        fake_device: FakeDevice,
    ) -> Result<StartAndServeTestHarness<impl Future<Output = Result<(), zx::Status>>>, zx::Status>
    {
        let fake_buffer_provider = wlan_mlme::buffer::FakeCBufferProvider::new();
        let (softmac_handle_sender, mut softmac_handle_receiver) =
            oneshot::channel::<Result<WlanSoftmacHandle, zx::Status>>();
        let start_and_serve_fut = start_and_serve(
            move |result: Result<WlanSoftmacHandle, zx::Status>| {
                softmac_handle_sender
                    .send(result)
                    .expect("Failed to signal initialization complete.")
            },
            fake_device.clone(),
            fake_buffer_provider,
        );
        let mut start_and_serve_fut = Box::pin(start_and_serve_fut);

        let usme_bootstrap_client_end = fake_device.state().lock().usme_bootstrap_client_end.take();
        match usme_bootstrap_client_end {
            // Simulate an errant initialization case where the UsmeBootstrap client end has been dropped
            // during initialization.
            None => match TestExecutor::poll_until_stalled(&mut start_and_serve_fut).await {
                Poll::Pending => panic!(
                    "start_and_serve() failed to panic when the UsmeBootstrap client was dropped."
                ),
                Poll::Ready(result) => {
                    // Assert the same initialization error appears in the receiver too.
                    let status = result.unwrap_err();
                    assert_eq!(
                        status,
                        assert_variant!(
                            TestExecutor::poll_until_stalled(&mut softmac_handle_receiver).await,
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
                let start_and_serve_fut = match TestExecutor::poll_until_stalled(
                    &mut start_and_serve_fut,
                )
                .await
                {
                    Poll::Pending => start_and_serve_fut,
                    Poll::Ready(result) => {
                        // Assert the same initialization error appears in the receiver too.
                        let status = result.unwrap_err();
                        assert_eq!(
                            status,
                            assert_variant!(
                                TestExecutor::poll_until_stalled(&mut softmac_handle_receiver).await,
                                Poll::Ready(Ok(Err(status))) => status
                            )
                        );
                        return Err(status);
                    }
                };

                inspect_vmo_fut.await.expect("Failed to bootstrap USME.");

                Ok(StartAndServeTestHarness {
                    start_and_serve_fut,
                    softmac_handle_receiver,
                    generic_sme_proxy,
                })
            }
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_and_serve_fails_on_dropped_usme_bootstrap_client() {
        let (fake_device, fake_device_state) = FakeDevice::new().await;
        fake_device_state.lock().usme_bootstrap_client_end = None;
        match start_and_serve_with_device(fake_device.clone()).await {
            Ok(_) => panic!(
                "start_and_serve() does not fail when the UsmeBootstrap client end is dropped."
            ),
            Err(status) => assert_eq!(status, zx::Status::INTERNAL),
        }
    }

    // Exhaustive feature tests are unit tested on start()
    #[fuchsia::test(allow_stalls = false)]
    async fn start_and_serve_fails_with_wrong_mac_implementation_type() {
        let (fake_device, _fake_device_state) = FakeDevice::new_with_config(
            FakeDeviceConfig::default()
                .with_mock_mac_implementation_type(fidl_common::MacImplementationType::Fullmac),
        )
        .await;

        match start_and_serve_with_device(fake_device).await {
            Ok(_) => panic!(
                "start_and_serve() future did not terminate before attempting bootstrap SME."
            ),
            Err(status) => assert_eq!(status, zx::Status::INTERNAL),
        };
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_and_serve_fails_on_dropped_mlme_event_stream() {
        let (mut fake_device, _fake_device_state) = FakeDevice::new().await;
        let _ = fake_device.take_mlme_event_stream();
        match start_and_serve_with_device(fake_device.clone()).await {
            Ok(_) => {
                panic!("start_and_serve() does not fail when the MLME event stream is missing.")
            }
            Err(status) => assert_eq!(status, zx::Status::INTERNAL),
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_and_serve_fails_on_dropped_generic_sme_client() {
        let (fake_device, _fake_device_state) = FakeDevice::new().await;
        let StartAndServeTestHarness {
            mut start_and_serve_fut,
            mut softmac_handle_receiver,
            generic_sme_proxy,
        } = start_and_serve_with_device(fake_device)
            .await
            .expect("Failed to initiate wlansoftmac setup.");
        let _handle = assert_variant!(TestExecutor::poll_until_stalled(&mut softmac_handle_receiver).await, Poll::Ready(Ok(Ok(handle))) => handle);
        assert_eq!(TestExecutor::poll_until_stalled(&mut start_and_serve_fut).await, Poll::Pending);

        drop(generic_sme_proxy);

        assert_eq!(
            TestExecutor::poll_until_stalled(&mut start_and_serve_fut).await,
            Poll::Ready(Err(zx::Status::INTERNAL))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_and_serve_shuts_down_gracefully() {
        let (fake_device, _fake_device_state) = FakeDevice::new().await;
        let StartAndServeTestHarness {
            mut start_and_serve_fut,
            mut softmac_handle_receiver,
            generic_sme_proxy: _generic_sme_proxy,
        } = start_and_serve_with_device(fake_device)
            .await
            .expect("Failed to initiate wlansoftmac setup.");
        assert_eq!(TestExecutor::poll_until_stalled(&mut start_and_serve_fut).await, Poll::Pending);
        let handle = assert_variant!(TestExecutor::poll_until_stalled(&mut softmac_handle_receiver).await, Poll::Ready(Ok(Ok(handle))) => handle);

        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        handle.stop(StopCompleter::new(Box::new(move || {
            shutdown_sender.send(()).expect("Failed to signal shutdown completion.")
        })));
        assert_variant!(futures::join!(start_and_serve_fut, shutdown_receiver), (Ok(()), Ok(())));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_and_serve_responds_to_generic_sme_requests() {
        let (fake_device, _fake_device_state) = FakeDevice::new().await;
        let StartAndServeTestHarness {
            mut start_and_serve_fut,
            mut softmac_handle_receiver,
            generic_sme_proxy,
        } = start_and_serve_with_device(fake_device)
            .await
            .expect("Failed to initiate wlansoftmac setup.");
        let handle = assert_variant!(TestExecutor::poll_until_stalled(&mut softmac_handle_receiver).await, Poll::Ready(Ok(Ok(handle))) => handle);

        let (sme_telemetry_proxy, sme_telemetry_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");
        let (client_sme_proxy, client_sme_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");

        let resp_fut = generic_sme_proxy.get_sme_telemetry(sme_telemetry_server);
        pin_mut!(resp_fut);

        // First poll `get_sme_telemetry` to send a `GetSmeTelemetry` request to the SME server, and then
        // poll the SME server process it. Finally, expect `get_sme_telemetry` to complete with `Ok(())`.
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Pending);
        assert_eq!(TestExecutor::poll_until_stalled(&mut start_and_serve_fut).await, Poll::Pending);
        assert_variant!(
            TestExecutor::poll_until_stalled(&mut resp_fut).await,
            Poll::Ready(Ok(Ok(())))
        );

        let resp_fut = generic_sme_proxy.get_client_sme(client_sme_server);
        pin_mut!(resp_fut);

        // First poll `get_client_sme` to send a `GetClientSme` request to the SME server, and then poll the
        // SME server process it. Finally, expect `get_client_sme` to complete with `Ok(())`.
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Pending);
        assert_eq!(TestExecutor::poll_until_stalled(&mut start_and_serve_fut).await, Poll::Pending);
        resp_fut.await.expect("Generic SME proxy failed").expect("Client SME request failed");

        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        handle.stop(StopCompleter::new(Box::new(move || {
            shutdown_sender.send(()).expect("Failed to signal shutdown completion.")
        })));
        assert_variant!(futures::join!(start_and_serve_fut, shutdown_receiver), (Ok(()), Ok(())));

        // All SME proxies should shutdown.
        assert!(generic_sme_proxy.is_closed());
        assert!(sme_telemetry_proxy.is_closed());
        assert!(client_sme_proxy.is_closed());
    }

    // Mocking a passive scan verifies the path through SME, MLME, and the FFI is functional. Other paths
    // are much more complex to mock and sufficiently covered by other testing. For example, queueing an
    // Ethernet frame requires mocking an association first, and the outcome of a reported Tx result cannot
    // be confirmed because the Minstrel is internal to MLME.
    #[fuchsia::test(allow_stalls = false)]
    async fn start_and_serve_responds_to_passive_scan_request() {
        let (fake_device, fake_device_state) = FakeDevice::new().await;
        let StartAndServeTestHarness {
            mut start_and_serve_fut,
            mut softmac_handle_receiver,
            generic_sme_proxy,
        } = start_and_serve_with_device(fake_device)
            .await
            .expect("Failed to initiate wlansoftmac setup.");
        let handle = assert_variant!(TestExecutor::poll_until_stalled(&mut softmac_handle_receiver).await, Poll::Ready(Ok(Ok(handle))) => handle);

        let (client_sme_proxy, client_sme_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");

        let resp_fut = generic_sme_proxy.get_client_sme(client_sme_server);
        pin_mut!(resp_fut);
        assert_variant!(TestExecutor::poll_until_stalled(&mut resp_fut).await, Poll::Pending);
        assert_eq!(TestExecutor::poll_until_stalled(&mut start_and_serve_fut).await, Poll::Pending);
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut resp_fut).await,
            Poll::Ready(Ok(Ok(())))
        ));

        let scan_response_fut =
            client_sme_proxy.scan(&fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {}));
        pin_mut!(scan_response_fut);
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut scan_response_fut).await,
            Poll::Pending
        ));

        assert!(fake_device_state.lock().captured_passive_scan_request.is_none());
        assert_eq!(TestExecutor::poll_until_stalled(&mut start_and_serve_fut).await, Poll::Pending);
        assert!(fake_device_state.lock().captured_passive_scan_request.is_some());

        let wlan_softmac_ifc_bridge_proxy =
            fake_device_state.lock().wlan_softmac_ifc_bridge_proxy.take().unwrap();
        let notify_scan_complete_fut = wlan_softmac_ifc_bridge_proxy.notify_scan_complete(
            &fidl_softmac::WlanSoftmacIfcBaseNotifyScanCompleteRequest {
                status: Some(zx::Status::OK.into_raw()),
                scan_id: Some(0),
                ..Default::default()
            },
        );
        notify_scan_complete_fut.await.expect("Failed to receive NotifyScanComplete response");
        assert_eq!(TestExecutor::poll_until_stalled(&mut start_and_serve_fut).await, Poll::Pending);
        assert!(matches!(
            TestExecutor::poll_until_stalled(&mut scan_response_fut).await,
            Poll::Ready(Ok(_))
        ));

        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        handle.stop(StopCompleter::new(Box::new(move || {
            shutdown_sender.send(()).expect("Failed to signal shutdown completion.")
        })));
        assert_variant!(futures::join!(start_and_serve_fut, shutdown_receiver), (Ok(()), Ok(())));

        // All SME proxies should shutdown.
        assert!(generic_sme_proxy.is_closed());
        assert!(client_sme_proxy.is_closed());
    }
}
