// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod capturer;
mod clock;
mod device;
mod error;
mod renderer;
mod ring_buffer;
mod wav_socket;

use anyhow::{anyhow, Context, Error};
use error::ControllerError;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_audio_controller as fac;
use fidl_fuchsia_hardware_audio as fhaudio;
use format_utils::Format;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter};
use fuchsia_zircon as zx;
use futures::{StreamExt, TryStreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::error;

const SECONDS_PER_NANOSECOND: f64 = 1.0 / 10_u64.pow(9) as f64;

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    DeviceControl(fac::DeviceControlRequestStream),
    Player(fac::PlayerRequestStream),
    Recorder(fac::RecorderRequestStream),
}

struct AudioDaemon {}

impl AudioDaemon {
    async fn play_device(
        &self,
        request: fac::PlayerPlayRequest,
    ) -> Result<fac::PlayerPlayResponse, Error> {
        let device_selector = request
            .destination
            .ok_or(anyhow!("Device id argument missing."))
            .and_then(|play_location| match play_location {
                fac::PlayDestination::DeviceRingBuffer(device_selector) => Ok(device_selector),
                _ => Err(anyhow!("Expected Ring Buffer play location")),
            })?;

        let data_socket = request.wav_source.ok_or(anyhow!("Socket argument missing."))?;
        let async_socket = fasync::Socket::from_socket(data_socket);

        let mut device = device::Device::new_from_selector(&device_selector)?;

        device.play(async_socket).await
    }

    async fn record_device(
        &self,
        request: fac::RecorderRecordRequest,
    ) -> Result<fac::RecorderRecordResponse, error::ControllerError> {
        let stream_type = request.stream_type.ok_or(error::ControllerError::new(
            fac::Error::ArgumentsMissing,
            format!("Stream type missing."),
        ))?;
        let wav_data = request.wav_data.ok_or(error::ControllerError::new(
            fac::Error::ArgumentsMissing,
            format!("Socket for wav data missing."),
        ))?;

        let device_selector = request
            .source
            .ok_or(error::ControllerError::new(
                fac::Error::ArgumentsMissing,
                format!("Record source missing."),
            ))
            .and_then(|location| match location {
                fac::RecordSource::DeviceRingBuffer(device_selector) => Ok(device_selector),
                unknown_source => Err(error::ControllerError::new(
                    fac::Error::InvalidArguments,
                    format!("Expected ring buffer source, found {unknown_source:?}"),
                )),
            })?;

        let cancel_server = request.canceler;
        let duration = request.duration.map(|duration| Duration::from_nanos(duration as u64));

        let mut device = device::Device::new_from_selector(&device_selector).map_err(|err| {
            error::ControllerError::new(
                fac::Error::DeviceNotReachable,
                format!("Failed to connect to device with error: {err}"),
            )
        })?;

        device
            .record(
                Format::from(&stream_type),
                fasync::Socket::from_socket(wav_data),
                duration,
                cancel_server,
            )
            .await
    }

    async fn serve_player(&mut self, mut stream: fac::PlayerRequestStream) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            let request_name = request.method_name();
            let result = match request {
                fac::PlayerRequest::Play { payload, responder } => {
                    let response = match payload.destination {
                        Some(fac::PlayDestination::Renderer(..)) => {
                            renderer::play_renderer(payload).await.map_err(|e| {
                                println!("Error trying to play to AudioRenderer {e}");
                                fac::Error::UnknownFatal
                            })
                        }
                        Some(fac::PlayDestination::DeviceRingBuffer(..)) => {
                            self.play_device(payload).await.map_err(|e| {
                                println!("Error trying to play to device ring buffer {e}");
                                fac::Error::UnknownFatal
                            })
                        }
                        Some(unknown_destination) => {
                            println!("Unsupported PlayDestination variant specified. Requested: {:?} not supported.", unknown_destination);
                            Err(fac::Error::InvalidArguments)
                        }
                        None => {
                            println!("Missing destination argument.");
                            Err(fac::Error::ArgumentsMissing)
                        }
                    };

                    responder.send(response).map_err(|e| anyhow!("Could not send reponse: {}", e))
                }
                _ => Err(anyhow!("Request {request_name} not supported.")),
            };

            match result {
                Ok(_) => println!("Request succeeded."),
                Err(e) => {
                    let error_msg = format!("Request {request_name} failed with error {e} \n");
                    println!("{}", &error_msg);
                }
            }
        }
        Ok(())
    }

    async fn serve_recorder(
        &mut self,
        mut stream: fac::RecorderRequestStream,
    ) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            let request_name = request.method_name();
            match request {
                fac::RecorderRequest::Record { payload, responder } => {
                    let record_result = match payload.source {
                        Some(fac::RecordSource::Capturer(..))
                        | Some(fac::RecordSource::Loopback(..)) => {
                            capturer::record_capturer(payload).await
                        }
                        Some(fac::RecordSource::DeviceRingBuffer(..)) => {
                            self.record_device(payload).await
                        }
                        Some(unknown_source) => {
                            let error_msg = format!("Unsupported RecordSource variant specified. Requested: {unknown_source:?} not supported.");
                            println!("{error_msg}");
                            Err(ControllerError::new(fac::Error::InvalidArguments, error_msg))
                        }
                        None => {
                            println!("RecordSource argument missing.");
                            Err(ControllerError::new(
                                fac::Error::ArgumentsMissing,
                                format!("RecordSource argument missing"),
                            ))
                        }
                    };
                    match record_result {
                        Ok(response) => {
                            println!("Request succeeded.");
                            responder
                                .send(Ok(response))
                                .map_err(|e| anyhow!("Could not send reponse: {e}"))
                        }
                        Err(e) => {
                            println!("Request {request_name} failed with error {e} \n");
                            responder
                                .send(Err(e.inner))
                                .map_err(|e| anyhow!("Could not send reponse: {e}"))
                        }
                    }
                }
                _ => Err(anyhow!("Request {request_name} not supported.")),
            }?;
        }
        Ok(())
    }

    // TODO(b/298683668) this will be removed, replaced by client direct calls.
    async fn serve_device_control(
        &mut self,
        mut stream: fac::DeviceControlRequestStream,
    ) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            let request_name = request.method_name();
            let request_result = match request {
                fac::DeviceControlRequest::ListDevices { responder } => {
                    let mut entries = Vec::<fac::DeviceSelector>::new();
                    let devfs_devices = [
                        (fhaudio::DeviceType::StreamConfig, "/dev/class/audio-input/", Some(true)),
                        (
                            fhaudio::DeviceType::StreamConfig,
                            "/dev/class/audio-output/",
                            Some(false),
                        ),
                        (fhaudio::DeviceType::Composite, "/dev/class/audio-composite/", None),
                    ];

                    for (device_type, path, is_input) in devfs_devices {
                        match device::get_entries(path, device_type, is_input).await {
                            Ok(mut device_entries) => entries.append(&mut device_entries),
                            Err(e) => {
                                println!("Failed to get {device_type:?} entries: {e}")
                            }
                        }
                    }

                    let response = fac::DeviceControlListDevicesResponse {
                        devices: Some(entries),
                        ..Default::default()
                    };
                    responder.send(Ok(response)).map_err(|e| anyhow!("Error sending response: {e}"))
                }
                fac::DeviceControlRequest::GetDeviceInfo { payload, responder } => {
                    let device_selector = payload.device.ok_or(anyhow!("No device specified"))?;

                    let mut device = device::Device::new_from_selector(&device_selector)?;

                    let info = device.get_info().await;
                    match info {
                        Ok(info) => {
                            let response = fac::DeviceControlGetDeviceInfoResponse {
                                device_info: Some(info),
                                ..Default::default()
                            };
                            responder
                                .send(Ok(response))
                                .map_err(|e| anyhow!("Error sending response: {e}"))
                        }
                        Err(e) => {
                            println!("Could not connect to device. {e}");
                            responder
                                .send(Err(zx::Status::INTERNAL.into_raw()))
                                .map_err(|e| anyhow!("Error sending response: {e}"))
                        }
                    }
                }
                fac::DeviceControlRequest::DeviceSetGainState { payload, responder } => {
                    let (device_selector, gain_state) = (
                        payload.device.ok_or(anyhow!("No device specified"))?,
                        payload.gain_state.ok_or(anyhow!("No gain state specified"))?,
                    );

                    let mut device = device::Device::new_from_selector(&device_selector)?;

                    device.set_gain(gain_state)?;
                    responder.send(Ok(())).map_err(|e| anyhow!("Error sending response: {e}"))
                }
                _ => Err(anyhow!("Request {request_name} not supported.")),
            };
            match request_result {
                Ok(_) => println!("Request succeeded."),
                Err(e) => {
                    let error_msg = format!("Request {request_name} failed with error {e} \n");
                    println!("{}", &error_msg);
                }
            }
        }
        Ok(())
    }
}

pub async fn stop_listener(
    canceler: ServerEnd<fac::RecordCancelerMarker>,
    stop_signal: &AtomicBool,
) -> Result<(), Error> {
    let mut stream = canceler
        .into_stream()
        .map_err(|e| anyhow!("Error turning canceler server into stream {}", e))?;

    match stream.try_next().await {
        Ok(Some(request)) => match request {
            fac::RecordCancelerRequest::Cancel { responder } => {
                stop_signal.store(true, Ordering::SeqCst);
                responder.send(Ok(())).context("FIDL error with stop request")
            }
            _ => Err(anyhow!("Request not supported.")),
        },
        Ok(None) | Err(_) => {
            stop_signal.store(true, Ordering::SeqCst);
            Err(anyhow!("FIDL error with stop request"))
        }
    }
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), Error> {
    let mut service_fs = ServiceFs::new_local();

    // Initialize inspect
    let _inspect_server_task = inspect_runtime::publish(
        component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );
    component::health().set_starting_up();

    // Add services here. E.g:
    service_fs.dir("svc").add_fidl_service(IncomingRequest::DeviceControl);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Player);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Recorder);
    service_fs.take_and_serve_directory_handle().context("Failed to serve outgoing namespace")?;

    component::health().set_ok();

    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            // Match on `request` and handle each protocol.
            let mut audio_daemon = AudioDaemon {};

            match request {
                IncomingRequest::DeviceControl(stream) => {
                    if let Err(err) = audio_daemon.serve_device_control(stream).await {
                        error!(%err, "Failed to serve DeviceControl protocol");
                    }
                }
                IncomingRequest::Player(stream) => {
                    if let Err(err) = audio_daemon.serve_player(stream).await {
                        error!(%err, "Failed to serve Player protocol");
                    }
                }
                IncomingRequest::Recorder(stream) => {
                    if let Err(err) = audio_daemon.serve_recorder(stream).await {
                        error!(%err, "Failed to serve Recorder protocol");
                    }
                }
            }
        })
        .await;

    Ok(())
}
