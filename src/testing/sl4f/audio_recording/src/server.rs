// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use fidl_fuchsia_test_audio::{
    AudioTestError, CaptureRequest, CaptureRequestStream, InjectionRequest, InjectionRequestStream,
    INJECTED_AUDIO_MAXIMUM_FILE_SIZE,
};
use fuchsia_async as fasync;
use futures::stream::TryStreamExt;
use futures::{AsyncReadExt, AsyncWriteExt};
use log::{debug, error, warn};
use std::sync::Arc;

use crate::audio_facade::AudioFacade;

pub async fn handle_injection_request(
    facade: Arc<AudioFacade>,
    stream: InjectionRequestStream,
) -> Result<(), fidl::Error> {
    stream
        .try_for_each(move |request| {
            let facade_clone = facade.clone();
            Box::pin(async move {
                match request {
                    InjectionRequest::ClearInputAudio { index, responder } => {
                        debug!("ClearInputAudio({})", index);
                        facade_clone.clear_input_audio(index.try_into().unwrap()).await;
                        responder.send(Ok(())).context("error sending response").unwrap();
                    }
                    InjectionRequest::StartInputInjection { index, responder } => {
                        debug!("StartInputInjection({})", index);
                        let result = match facade_clone
                            .start_input_injection(index.try_into().unwrap())
                            .await
                        {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                error!("StartInputInjection failed: {:?}", e);
                                Err(AudioTestError::Fail)
                            }
                        };
                        responder.send(result).context("error sending response").unwrap();
                    }
                    InjectionRequest::StopInputInjection { responder } => {
                        debug!("StopInputInjection");
                        let result = match facade_clone.stop_input_injection().await {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                error!("StopInputInjection failed: {:?}", e);
                                Err(AudioTestError::Fail)
                            }
                        };
                        responder.send(result).context("error sending response").unwrap();
                    }
                    InjectionRequest::WriteInputAudio { index, audio_writer, ..} => {
                        debug!("WriteInputAudio({})", index);
                        let mut audio_stream = fasync::Socket::from_socket(audio_writer);
                        let mut read_bytes = 0;
                        let mut audio_data = Vec::new();
                        let mut local_buf = [0u8; 1024 * 16];

                        while let Ok(val) = audio_stream.read(&mut local_buf).await {
                            debug!("Got {} bytes from audio stream", val);
                            if val == 0 {
                                break;
                            }
                            read_bytes += val;
                            if read_bytes > INJECTED_AUDIO_MAXIMUM_FILE_SIZE as usize {
                                error!("Attempted to write too many bytes to audio injection. Limit is {INJECTED_AUDIO_MAXIMUM_FILE_SIZE} but already tried to write {read_bytes}");
                            }
                            audio_data.extend_from_slice(&local_buf[..val]);
                        }

                        debug!("Done reading audio stream");

                        facade_clone.put_input_audio( index.try_into().unwrap(), audio_data)
                        .await
                        .context("put_input_audio errored")
                        .unwrap();
                    }
                    InjectionRequest::GetInputAudioSize { index, responder } => {
                        debug!("GetInputAudioSize({})", index);
                        let result = match facade_clone
                            .get_input_audio_size(index.try_into().unwrap()).await {
                                Ok(size) => Ok(size.try_into().unwrap()),
                                Err(e) => {
                                    error!("GetInputAudioSize failed: {:?}", e);
                                    Err(AudioTestError::Fail)
                                }
                            };
                        responder.send(result).context("error sending response").unwrap();
                    }
                    InjectionRequest::WaitUntilInputIsDone { responder } => {
                        debug!("WaitUntilInputIsDone...");
                        let response = match facade_clone.wait_until_input_playing_is_finished().await {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                error!("WaitUntilInputIsDone failed: {:?}", e);
                                Err(AudioTestError::Fail)
                            }
                        };
                        responder.send(response).context("error sending response").unwrap();
                    }
                    InjectionRequest::_UnknownMethod { ordinal, .. } => {
                        error!("Unknown method received, ordinal {ordinal}");
                    }
                }
                Ok(())
            })
        })
        .await
}

pub async fn handle_capture_request(
    facade: Arc<AudioFacade>,
    stream: CaptureRequestStream,
) -> Result<(), fidl::Error> {
    stream
        .try_for_each(move |request| {
            let facade_clone = facade.clone();
            Box::pin(async move {
                match request {
                    CaptureRequest::GetOutputAudio { responder } => {
                        let result = match facade_clone
                            .get_output_audio_vec()
                            .await
                            .context("get_output_audio errored")
                        {
                            Ok(data) => data,
                            Err(e) => {
                                error!("GetOutputAudio failed: {:?}", e);
                                responder
                                    .send(Err(AudioTestError::Fail))
                                    .context("error sending response")
                                    .unwrap();
                                return Ok(());
                            }
                        };
                        let (sender, receiver) = zx::Socket::create_stream();
                        sender.half_close().expect("prevent writes to receiver");
                        fasync::Task::spawn(async move {
                            debug!("Starting output audio stream");
                            let mut sender = fasync::Socket::from_socket(sender);
                            if let Err(e) = sender.write_all(result.as_slice()).await {
                                warn!("Failed to write audio output stream: {e:?}");
                            } else {
                                debug!(
                                    "Finished output audio stream, wrote {} bytes",
                                    result.len()
                                );
                            }
                        })
                        .detach();
                        responder.send(Ok(receiver)).context("error sending response").unwrap();
                    }
                    CaptureRequest::StartOutputCapture { responder } => {
                        debug!("StartOutputSave");
                        let result = match facade_clone.start_output_capture().await {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                error!("StartOutputSave failed: {:?}", e);
                                Err(AudioTestError::Fail)
                            }
                        };
                        responder.send(result).context("error sending response").unwrap();
                    }
                    CaptureRequest::StopOutputCapture { responder } => {
                        debug!("StopOutputSave");
                        let result = match facade_clone.stop_output_capture().await {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                error!("StopOutputSave failed: {:?}", e);
                                Err(AudioTestError::Fail)
                            }
                        };
                        responder.send(result).context("error sending response").unwrap();
                    }
                    CaptureRequest::_UnknownMethod { ordinal, .. } => {
                        error!("Unknown method received, ordinal {ordinal}");
                    }
                }
                Ok(())
            })
        })
        .await
}
