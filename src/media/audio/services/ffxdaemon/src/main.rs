// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, Context, Error},
    async_lock as _,
    fidl::HandleBased,
    fidl_fuchsia_audio_ffxdaemon::{
        AudioDaemonPlayRequest, AudioDaemonPlayResponder, AudioDaemonPlayResponse,
        AudioDaemonRequest, AudioDaemonRequestStream, PlayLocation,
    },
    fidl_fuchsia_media::{AudioRendererProxy, AudioStreamType},
    fidl_fuchsia_media_audio as _, fuchsia as _, fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_inspect::{component, health::Reporter},
    fuchsia_zircon::{self as zx},
    futures::future::{BoxFuture, FutureExt},
    futures::prelude::*,
    futures::StreamExt,
    hound,
    std::cmp,
    std::io::Cursor,
    std::rc::Rc,
};

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    AudioDaemon(AudioDaemonRequestStream),
}
struct AudioDaemon<'a> {
    // Keep daemon and audio proxy around with same lifetime. Avoid dangling reference
    audio: &'a fidl_fuchsia_media::AudioProxy,
}

impl<'a> AudioDaemon<'a> {
    pub fn new(audio_component: &'a fidl_fuchsia_media::AudioProxy) -> Self {
        Self { audio: audio_component }
    }

    fn send_next_packet<'b>(
        payload_offset: u64,
        mut socket: fidl::AsyncSocket,
        vmo: zx::Vmo,
        audio_renderer_proxy: &'b AudioRendererProxy,
        bytes_per_packet: usize,
        iteration: u32,
    ) -> BoxFuture<'b, Result<(), Error>> {
        async move {
            let mut total_bytes_read = 0;
            let mut buf = vec![0u8; bytes_per_packet];

            loop {
                let bytes_read = socket.read(&mut buf[total_bytes_read..]).await?; // slide window if last read didnt fill entire buffer.
                total_bytes_read += bytes_read;
                if bytes_read == 0 || total_bytes_read == bytes_per_packet {
                    break;
                }
            }

            if total_bytes_read == 0 {
                return Ok(());
            }
            vmo.write(&buf[..total_bytes_read], payload_offset as u64)?;

            let packet_fut =
                audio_renderer_proxy.send_packet(&mut fidl_fuchsia_media::StreamPacket {
                    pts: fidl_fuchsia_media::NO_TIMESTAMP,
                    payload_buffer_id: 0,
                    payload_offset: payload_offset as u64,
                    payload_size: total_bytes_read as u64,
                    flags: 0,
                    buffer_config: 0,
                    stream_segment_id: 0,
                });

            if payload_offset == 0 && iteration == 1 {
                audio_renderer_proxy
                    .play(fidl_fuchsia_media::NO_TIMESTAMP, fidl_fuchsia_media::NO_TIMESTAMP)
                    .await?;
            }

            packet_fut.await?;

            if total_bytes_read == bytes_per_packet {
                Self::send_next_packet(
                    payload_offset,
                    socket,
                    vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                    &audio_renderer_proxy,
                    bytes_per_packet,
                    iteration + 1,
                )
                .await?;
            } else {
                return Ok(());
            }
            Ok(())
        }
        .boxed()
    }

    async fn play_renderer(
        &self,
        request: AudioDaemonPlayRequest,
        responder: AudioDaemonPlayResponder,
    ) -> Result<(), anyhow::Error> {
        let num_packets = 4;
        let (stdout_remote, stdout_local) = zx::Socket::create(zx::SocketOpts::STREAM)?;

        let (stderr_remote, _stderr_local) = zx::Socket::create(zx::SocketOpts::STREAM)?;

        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_media::AudioRendererMarker>()?;

        let (gain_control_client_end, gain_control_server_end) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_media_audio::GainControlMarker>()?;
        let data_socket = request.socket.ok_or(anyhow::anyhow!("Socket argument missing."))?;

        self.audio.create_audio_renderer(server_end)?;
        let audio_renderer_proxy = Rc::new(client_end.into_proxy()?);

        let response = AudioDaemonPlayResponse {
            stdout: Some(stdout_remote),
            stderr: Some(stderr_remote),
            ..AudioDaemonPlayResponse::EMPTY
        };

        let spec = {
            let mut header_buf = vec![0u8; 44];
            fasync::Socket::from_socket(data_socket.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?
                .read_exact(&mut header_buf)
                .await?;
            let cursor_header = Cursor::new(header_buf);
            let reader = hound::WavReader::new(cursor_header.clone())?;
            reader.spec()
        };

        let (bytes_per_sample, sample_format) = match spec.sample_format {
            hound::SampleFormat::Int => match spec.bits_per_sample {
                0..=8 => (1, fidl_fuchsia_media::AudioSampleFormat::Unsigned8),
                9..=16 => (2, fidl_fuchsia_media::AudioSampleFormat::Signed16),
                17..=32 => (4, fidl_fuchsia_media::AudioSampleFormat::Signed24In32),
                33.. => panic!("Unsupported bits per sample."),
            },
            hound::SampleFormat::Float => (4, fidl_fuchsia_media::AudioSampleFormat::Float),
        };

        let mut pcm_stream_type = AudioStreamType {
            sample_format,
            channels: spec.channels as u32,
            frames_per_second: spec.sample_rate,
        };

        let vmo_size_bytes = spec.sample_rate as usize * bytes_per_sample as usize;
        let vmo = zx::Vmo::create(vmo_size_bytes as u64)?;

        let bytes_per_packet = cmp::min(vmo_size_bytes / num_packets as usize, 32000 as usize);
        let location = request.location.ok_or(anyhow::anyhow!("Location missing"))?;

        let usage = match location {
            PlayLocation::Renderer(renderer_info) => {
                renderer_info.usage.ok_or(anyhow::anyhow!("No usage provided."))?
            }
            _ => panic!("Expected Renderer PlayLocation"),
        };

        audio_renderer_proxy.set_usage(usage)?;

        audio_renderer_proxy.set_pcm_stream_type(&mut pcm_stream_type)?;

        audio_renderer_proxy
            .add_payload_buffer(0, vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;

        audio_renderer_proxy.bind_gain_control(gain_control_server_end)?;

        let gain_control_proxy = gain_control_client_end.into_proxy()?;
        let (gain_db, mute) = {
            let settings =
                request.gain_settings.ok_or(anyhow::anyhow!("Gain settings argument missing."))?;
            (
                settings.gain.ok_or(anyhow::anyhow!("Gain value not specified."))?,
                settings.mute.ok_or(anyhow::anyhow!("Mute option not specified."))?,
            )
        };
        gain_control_proxy.set_gain(gain_db)?;
        gain_control_proxy.set_mute(mute)?;

        audio_renderer_proxy.enable_min_lead_time_events(true)?;

        // Wait for AudioRenderer to initialize (lead_time > 0)
        let mut stream = audio_renderer_proxy.take_event_stream();
        while let Some(event) = stream.try_next().await? {
            match event {
                fidl_fuchsia_media::AudioRendererEvent::OnMinLeadTimeChanged {
                    min_lead_time_nsec,
                } => {
                    if min_lead_time_nsec > 0 {
                        break;
                    }
                }
            }
        }

        responder.send(&mut Ok(response)).expect("Failed to send play response.");

        let offsets: Vec<usize> = (0..num_packets).map(|x| x * bytes_per_packet).collect();

        let futs = offsets.iter().map(|offset| async {
            Self::send_next_packet(
                offset.to_owned() as u64,
                fasync::Socket::from_socket(
                    data_socket.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                )?,
                vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                &audio_renderer_proxy,
                bytes_per_packet,
                1,
            )
            .await?;
            Ok::<(), Error>(())
        });

        futures::future::try_join_all(futs).await?;

        let mut async_stdout =
            fasync::Socket::from_socket(stdout_local).expect("Async socket create failed.");
        async_stdout
            .write_all("Succesfully processed all audio data. \n".as_bytes())
            .await
            .expect("Write to socket failed.");
        Ok(())
    }

    async fn serve(&mut self, mut stream: AudioDaemonRequestStream) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                AudioDaemonRequest::Play { payload, responder } => match payload.location {
                    Some(PlayLocation::Renderer(..)) => {
                        self.play_renderer(payload, responder).await?;
                        Ok(())
                    }
                    Some(..) => Err(anyhow::anyhow!("No PlayLocation variant specified.")),
                    None => Err(anyhow::anyhow!("PlayLocation argument missing. ")),
                }?,
            }
        }
        Ok(())
    }
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), anyhow::Error> {
    let mut service_fs = ServiceFs::new_local();
    let audio_component =
        fuchsia_component::client::connect_to_protocol::<fidl_fuchsia_media::AudioMarker>()
            .context("Failed to connect to fuchsia.media.Audio")?;

    // Initialize inspect
    inspect_runtime::serve(component::inspector(), &mut service_fs)?;
    component::health().set_starting_up();

    // Add services here. E.g:
    service_fs.dir("svc").add_fidl_service(IncomingRequest::AudioDaemon);
    service_fs.take_and_serve_directory_handle().context("Failed to serve outgoing namespace")?;

    component::health().set_ok();

    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            // match on `request` and handle each protocol.
            let mut audio_daemon = AudioDaemon::new(&audio_component);

            match request {
                IncomingRequest::AudioDaemon(stream) => {
                    audio_daemon.serve(stream).await.unwrap_or_else(|e: Error| {
                        panic!("Couldn't serve audio daemon requests{:?}", e)
                    })
                }
            }
        })
        .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[fuchsia::test]
    async fn smoke_test() {
        assert!(true);
    }
}
