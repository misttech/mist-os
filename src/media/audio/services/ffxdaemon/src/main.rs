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

use capturer::Capturer;
use device::Device;
use renderer::Renderer;
use wav_socket::WavSocket;

use anyhow::{anyhow, Context, Error};
use error::ControllerError;
use fidl_fuchsia_audio_controller as fac;
use fidl_fuchsia_hardware_audio as fhaudio;
use fuchsia_async as fasync;
use fuchsia_audio::Format;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter};
use fuchsia_zircon as zx;
use futures::{StreamExt, TryStreamExt};
use std::time::Duration;
use tracing::error;

/// Wraps all hosted protocols into a single type that can be matched against and dispatched.
enum IncomingRequest {
    DeviceControl(fac::DeviceControlRequestStream),
    Player(fac::PlayerRequestStream),
    Recorder(fac::RecorderRequestStream),
}

pub async fn handle_play_request(
    request: fac::PlayerPlayRequest,
) -> Result<fac::PlayerPlayResponse, ControllerError> {
    let destination = request.destination.ok_or_else(|| {
        ControllerError::new(fac::Error::ArgumentsMissing, format!("destination missing"))
    })?;
    let mut wav_socket = {
        let wav_source = request.wav_source.ok_or_else(|| {
            ControllerError::new(fac::Error::ArgumentsMissing, format!("wav_source missing"))
        })?;
        WavSocket(fasync::Socket::from_socket(wav_source))
    };

    match destination {
        fac::PlayDestination::Renderer(config) => {
            let spec = wav_socket.read_header().await?;
            let format = Format::from(&spec);

            let renderer =
                Renderer::new(config, format, request.gain_settings).await.map_err(|err| {
                    ControllerError::new(
                        fac::Error::DeviceNotReachable,
                        format!("Failed to connect to renderer: {err}"),
                    )
                })?;

            renderer.play(wav_socket).await
        }
        fac::PlayDestination::DeviceRingBuffer(device_selector) => {
            let mut device = Device::new_from_selector(&device_selector).map_err(|err| {
                ControllerError::new(
                    fac::Error::DeviceNotReachable,
                    format!("Failed to connect to device with error: {err}"),
                )
            })?;
            device.play(wav_socket).await
        }
        fac::PlayDestinationUnknown!() => Err(ControllerError::new(
            fac::Error::InvalidArguments,
            format!("Unknown PlayDestination"),
        )),
    }
}

async fn serve_player(mut stream: fac::PlayerRequestStream) -> Result<(), Error> {
    while let Ok(Some(request)) = stream.try_next().await {
        let request_name = request.method_name();
        match request {
            fac::PlayerRequest::Play { payload, responder } => {
                let result = handle_play_request(payload).await;

                if let Err(ref err) = result {
                    error!(%err, "Failed to play");
                }
                responder.send(result.map_err(|err| err.inner)).context("Could not send response")
            }
            _ => Err(anyhow!("Unknown request {request_name}")),
        }?;
    }
    Ok(())
}

pub async fn handle_record_request(
    request: fac::RecorderRecordRequest,
) -> Result<fac::RecorderRecordResponse, ControllerError> {
    let source = request.source.ok_or_else(|| {
        ControllerError::new(fac::Error::ArgumentsMissing, format!("source missing"))
    })?;
    let stream_type = request.stream_type.ok_or_else(|| {
        ControllerError::new(fac::Error::ArgumentsMissing, format!("stream_type missing"))
    })?;
    let wav_socket = {
        let wav_data = request.wav_data.ok_or_else(|| {
            ControllerError::new(fac::Error::ArgumentsMissing, format!("wav_data missing"))
        })?;
        WavSocket(fasync::Socket::from_socket(wav_data))
    };
    let duration = request.duration.map(|duration| Duration::from_nanos(duration as u64));

    match source {
        fac::RecordSource::Capturer(..) | fac::RecordSource::Loopback(..) => {
            let mut capturer =
                Capturer::new(source, stream_type, request.gain_settings).await.map_err(|err| {
                    ControllerError::new(
                        fac::Error::DeviceNotReachable,
                        format!("Failed to connect to capturer: {err}"),
                    )
                })?;

            capturer.record(wav_socket, duration, request.canceler, request.buffer_size).await
        }
        fac::RecordSource::DeviceRingBuffer(selector) => {
            let format = Format::from(&stream_type);
            let mut device = Device::new_from_selector(&selector).map_err(|err| {
                ControllerError::new(
                    fac::Error::DeviceNotReachable,
                    format!("Failed to connect to device with error: {err}"),
                )
            })?;

            device.record(format, wav_socket, duration, request.canceler).await
        }
        fac::RecordSourceUnknown!() => {
            Err(ControllerError::new(fac::Error::InvalidArguments, format!("Unknown RecordSource")))
        }
    }
}

async fn serve_recorder(mut stream: fac::RecorderRequestStream) -> Result<(), Error> {
    while let Ok(Some(request)) = stream.try_next().await {
        let request_name = request.method_name();
        match request {
            fac::RecorderRequest::Record { payload, responder } => {
                let result = handle_record_request(payload).await;

                if let Err(ref err) = result {
                    error!(%err, "Failed to record");
                }
                responder.send(result.map_err(|err| err.inner)).context("Could not send response")
            }
            _ => Err(anyhow!("Unknown request {request_name}")),
        }?;
    }
    Ok(())
}

// TODO(b/298683668) this will be removed, replaced by client direct calls.
async fn serve_device_control(mut stream: fac::DeviceControlRequestStream) -> Result<(), Error> {
    while let Ok(Some(request)) = stream.try_next().await {
        let request_name = request.method_name();
        let request_result = match request {
            fac::DeviceControlRequest::ListDevices { responder } => {
                let mut entries = Vec::<fac::DeviceSelector>::new();
                let devfs_devices = [
                    (fhaudio::DeviceType::StreamConfig, "/dev/class/audio-input/", Some(true)),
                    (fhaudio::DeviceType::StreamConfig, "/dev/class/audio-output/", Some(false)),
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
            match request {
                IncomingRequest::DeviceControl(stream) => {
                    if let Err(err) = serve_device_control(stream).await {
                        error!(%err, "Failed to serve DeviceControl protocol");
                    }
                }
                IncomingRequest::Player(stream) => {
                    if let Err(err) = serve_player(stream).await {
                        error!(%err, "Failed to serve Player protocol");
                    }
                }
                IncomingRequest::Recorder(stream) => {
                    if let Err(err) = serve_recorder(stream).await {
                        error!(%err, "Failed to serve Recorder protocol");
                    }
                }
            }
        })
        .await;

    Ok(())
}
