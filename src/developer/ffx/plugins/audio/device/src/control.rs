// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_command::{user_error, FfxContext};
use fidl_fuchsia_audio_device as fadevice;
use fidl_fuchsia_hardware_audio as fhaudio;
use fuchsia_audio::dai::DaiFormat;
use fuchsia_zircon_status::Status;

#[async_trait]
pub trait DeviceControl {
    async fn set_dai_format(
        &self,
        dai_format: DaiFormat,
        element_id: Option<fadevice::ElementId>,
    ) -> fho::Result<()>;
}

pub struct HardwareCodec(pub fhaudio::CodecProxy);

#[async_trait]
impl DeviceControl for HardwareCodec {
    async fn set_dai_format(
        &self,
        dai_format: DaiFormat,
        _element_id: Option<fadevice::ElementId>,
    ) -> fho::Result<()> {
        let _ = self
            .0
            .set_dai_format(&dai_format.into())
            .await
            .bug_context("Failed to call SetDaiFormat")?
            .map_err(|status| Status::from_raw(status))
            .user_message("failed to set DAI format")?;
        Ok(())
    }
}

pub struct HardwareComposite(pub fhaudio::CompositeProxy);

#[async_trait]
impl DeviceControl for HardwareComposite {
    async fn set_dai_format(
        &self,
        dai_format: DaiFormat,
        element_id: Option<fadevice::ElementId>,
    ) -> fho::Result<()> {
        let element_id = element_id
            .ok_or_else(|| user_error!("element ID is required for Composite devices"))?;
        self.0
            .set_dai_format(element_id, &dai_format.into())
            .await
            .bug_context("Failed to call SetDaiFormat")?
            .map_err(|err| user_error!("failed to set DAI format: {:?}", err))?;
        Ok(())
    }
}

pub struct HardwareDai(#[allow(unused)] pub fhaudio::DaiProxy);

#[async_trait]
impl DeviceControl for HardwareDai {
    async fn set_dai_format(
        &self,
        _dai_format: DaiFormat,
        _element_id: Option<fadevice::ElementId>,
    ) -> fho::Result<()> {
        Err(user_error!("set dai-format is not supported for DAI devices"))
    }
}

pub struct HardwareStreamConfig(#[allow(unused)] pub fhaudio::StreamConfigProxy);

#[async_trait]
impl DeviceControl for HardwareStreamConfig {
    async fn set_dai_format(
        &self,
        _dai_format: DaiFormat,
        _element_id: Option<fadevice::ElementId>,
    ) -> fho::Result<()> {
        Err(user_error!("set dai-format is not supported for StreamConfig devices"))
    }
}

pub struct Registry(pub fadevice::ControlProxy);

#[async_trait]
impl DeviceControl for Registry {
    async fn set_dai_format(
        &self,
        dai_format: DaiFormat,
        element_id: Option<fadevice::ElementId>,
    ) -> fho::Result<()> {
        let _ = self
            .0
            .set_dai_format(&fadevice::ControlSetDaiFormatRequest {
                element_id,
                dai_format: Some(dai_format.into()),
                ..Default::default()
            })
            .await
            .bug_context("Failed to call SetDaiFormat")?
            .map_err(|err| user_error!("failed to set DAI format: {:?}", err))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::spawn_stream_handler;
    use fuchsia_audio::{
        dai::{DaiFrameFormat, DaiFrameFormatStandard, DaiSampleFormat},
        format::{SampleSize, BITS_16, BITS_32},
    };

    fn serve_hw_codec() -> fhaudio::CodecProxy {
        spawn_stream_handler(move |request| async move {
            match request {
                fhaudio::CodecRequest::SetDaiFormat { format: _, responder } => {
                    responder
                        .send(Ok(&fhaudio::CodecFormatInfo {
                            external_delay: None,
                            turn_on_delay: None,
                            turn_off_delay: None,
                            ..Default::default()
                        }))
                        .unwrap();
                }
                _ => unimplemented!(),
            }
        })
        .unwrap()
    }

    fn serve_hw_composite() -> fhaudio::CompositeProxy {
        spawn_stream_handler(move |request| async move {
            match request {
                fhaudio::CompositeRequest::SetDaiFormat {
                    processing_element_id: _,
                    format: _,
                    responder,
                } => {
                    responder.send(Ok(())).unwrap();
                }
                _ => unimplemented!(),
            }
        })
        .unwrap()
    }

    fn serve_hw_dai() -> fhaudio::DaiProxy {
        spawn_stream_handler(move |request| async move {
            match request {
                _ => unimplemented!(),
            }
        })
        .unwrap()
    }

    fn serve_hw_streamconfig() -> fhaudio::StreamConfigProxy {
        spawn_stream_handler(move |request| async move {
            match request {
                _ => unimplemented!(),
            }
        })
        .unwrap()
    }

    fn serve_registry_control() -> fadevice::ControlProxy {
        spawn_stream_handler(move |request| async move {
            match request {
                fadevice::ControlRequest::SetDaiFormat { payload, responder } => {
                    assert!(payload.dai_format.is_some());
                    assert!(payload.element_id.is_some());

                    responder
                        .send(Ok(&fadevice::ControlSetDaiFormatResponse {
                            state: Some(fhaudio::CodecFormatInfo {
                                external_delay: None,
                                turn_on_delay: None,
                                turn_off_delay: None,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }))
                        .unwrap();
                }
                _ => unimplemented!(),
            }
        })
        .unwrap()
    }

    #[fuchsia::test]
    async fn test_set_dai_format() {
        let dai_format = DaiFormat {
            number_of_channels: 2,
            channels_to_use_bitmask: 0x3,
            sample_format: DaiSampleFormat::PcmSigned,
            frame_format: DaiFrameFormat::Standard(DaiFrameFormatStandard::I2S),
            frame_rate: 48000,
            sample_size: SampleSize::from_partial_bits(BITS_16, BITS_32).unwrap(),
        };
        let element_id = Some(1);

        let codec: Box<dyn DeviceControl> = Box::new(HardwareCodec(serve_hw_codec()));
        assert!(codec.set_dai_format(dai_format, element_id).await.is_ok());

        let composite: Box<dyn DeviceControl> = Box::new(HardwareComposite(serve_hw_composite()));
        assert!(composite.set_dai_format(dai_format, element_id).await.is_ok());

        let dai: Box<dyn DeviceControl> = Box::new(HardwareDai(serve_hw_dai()));
        assert!(dai.set_dai_format(dai_format, element_id).await.is_err());

        let streamconfig: Box<dyn DeviceControl> =
            Box::new(HardwareStreamConfig(serve_hw_streamconfig()));
        assert!(streamconfig.set_dai_format(dai_format, element_id).await.is_err());

        let registry: Box<dyn DeviceControl> = Box::new(Registry(serve_registry_control()));
        assert!(registry.set_dai_format(dai_format, element_id).await.is_ok());
    }
}
