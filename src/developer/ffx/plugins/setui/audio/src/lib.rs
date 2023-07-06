// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_setui_audio_args::Audio;
use fho::{moniker, AvailabilityFlag, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_settings::{AudioProxy, AudioSettings};
use utils::handle_mixed_result;
use utils::{self, Either, WatchOrSetResult};

#[derive(FfxTool)]
#[check(AvailabilityFlag("setui"))]
pub struct AudioTool {
    #[command]
    cmd: Audio,
    #[with(moniker("/core/setui_service"))]
    audio_proxy: AudioProxy,
}

fho::embedded_plugin!(AudioTool);

#[async_trait(?Send)]
impl FfxMain for AudioTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        run_command(self.audio_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn run_command<W: std::io::Write>(
    audio_proxy: AudioProxy,
    audio: Audio,
    writer: &mut W,
) -> Result<()> {
    handle_mixed_result("Audio", command(audio_proxy, audio).await, writer).await
}

async fn command(proxy: AudioProxy, audio: Audio) -> WatchOrSetResult {
    let settings = AudioSettings::try_from(audio).expect("Iuput arguments have errors");

    if settings == AudioSettings::default() {
        Ok(Either::Watch(utils::watch_to_stream(proxy, |p| p.watch())))
    } else {
        Ok(Either::Set(if let Err(err) = proxy.set(&settings).await? {
            format!("{:?}", err)
        } else {
            format!("Successfully set Audio to {:?}", audio)
        }))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_media::AudioRenderUsage;
    use fidl_fuchsia_settings::AudioRequest;
    use test_case::test_case;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_command() {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            AudioRequest::Set { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            AudioRequest::Watch { .. } => {
                panic!("Unexpected call to watch");
            }
        });

        let audio = Audio {
            stream: Some(AudioRenderUsage::Background),
            source: Some(fidl_fuchsia_settings::AudioStreamSettingSource::User),
            level: Some(0.5),
            volume_muted: Some(false),
        };
        let response = run_command(proxy, audio, &mut vec![]).await;
        assert!(response.is_ok());
    }

    #[test_case(
        Audio {
            stream: Some(AudioRenderUsage::Media),
            source: Some(fidl_fuchsia_settings::AudioStreamSettingSource::User),
            level: Some(0.6),
            volume_muted: Some(false),
        };
        "Test audio set() output with non-empty input."
    )]
    #[test_case(
        Audio {
            stream: Some(AudioRenderUsage::Background),
            source: Some(fidl_fuchsia_settings::AudioStreamSettingSource::User),
            level: Some(0.1),
            volume_muted: Some(false),
        };
        "Test audio set() output with a different non-empty input."
    )]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn validate_audio_set_output(expected_audio: Audio) -> Result<()> {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            AudioRequest::Set { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            AudioRequest::Watch { .. } => {
                panic!("Unexpected call to watch");
            }
        });

        let output = utils::assert_set!(command(proxy, expected_audio));
        assert_eq!(output, format!("Successfully set Audio to {:?}", expected_audio));
        Ok(())
    }

    #[test_case(
        Audio {
            stream: None,
            source: None,
            level: None,
            volume_muted: None,
        };
        "Test audio watch() output with empty input."
    )]
    #[test_case(
        Audio {
            stream: Some(AudioRenderUsage::Background),
            source: Some(fidl_fuchsia_settings::AudioStreamSettingSource::User),
            level: Some(0.1),
            volume_muted: Some(false),
        };
        "Test audio watch() output with non-empty input."
    )]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn validate_audio_watch_output(expected_audio: Audio) -> Result<()> {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            AudioRequest::Set { .. } => {
                panic!("Unexpected call to set");
            }
            AudioRequest::Watch { responder } => {
                let _ = responder.send(
                    &AudioSettings::try_from(expected_audio).expect("Invalid input arguments"),
                );
            }
        });

        let output = utils::assert_watch!(command(
            proxy,
            Audio { stream: None, source: None, level: None, volume_muted: None }
        ));
        assert_eq!(
            output,
            format!(
                "{:#?}",
                AudioSettings::try_from(expected_audio).expect("Invalid input arguments")
            )
        );
        Ok(())
    }
}
