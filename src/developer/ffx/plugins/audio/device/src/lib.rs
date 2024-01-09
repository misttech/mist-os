// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    async_trait::async_trait,
    blocking::Unblock,
    errors::ffx_bail,
    ffx_audio_common::DeviceResult,
    ffx_audio_device_args::{DeviceCommand, DeviceDirection, SubCommand},
    fho::{moniker, FfxMain, FfxTool, MachineWriter, ToolIO},
    fidl::{endpoints::ServerEnd, HandleBased},
    fidl_fuchsia_audio_controller::{
        DeviceControlDeviceSetGainStateRequest, DeviceControlGetDeviceInfoRequest,
        DeviceControlProxy, DeviceInfo, DeviceSelector, PlayerPlayRequest, PlayerProxy,
        RecordCancelerMarker, RecordSource, RecorderProxy, RecorderRecordRequest,
    },
    fidl_fuchsia_hardware_audio::{PcmSupportedFormats, PlugDetectCapabilities},
    fidl_fuchsia_media::AudioStreamType,
    fuchsia_zircon_status::Status,
    futures::AsyncWrite,
    futures::FutureExt,
    serde::{Deserialize, Serialize},
    std::io::Read,
    std::marker::Send,
};

#[derive(FfxTool)]
pub struct DeviceTool {
    #[command]
    cmd: DeviceCommand,
    #[with(moniker("/core/audio_ffx_daemon"))]
    device_controller: DeviceControlProxy,
    #[with(moniker("/core/audio_ffx_daemon"))]
    record_controller: RecorderProxy,
    #[with(moniker("/core/audio_ffx_daemon"))]
    play_controller: PlayerProxy,
}

fho::embedded_plugin!(DeviceTool);
#[async_trait(?Send)]
impl FfxMain for DeviceTool {
    type Writer = MachineWriter<DeviceResult>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        match &self.cmd.subcommand {
            SubCommand::Info(_) => {
                device_info(self.device_controller, self.cmd).await.map_err(Into::into)
            }

            SubCommand::Play(play_command) => {
                let (play_remote, play_local) = fidl::Socket::create_datagram();
                let reader: Box<dyn Read + Send + 'static> = match &play_command.file {
                    Some(input_file_path) => {
                        let file = std::fs::File::open(&input_file_path).map_err(|e| {
                            anyhow::anyhow!("Error trying to open file \"{input_file_path}\": {e}")
                        })?;
                        Box::new(file)
                    }
                    None => Box::new(std::io::stdin()),
                };

                device_play(
                    self.device_controller,
                    self.play_controller,
                    self.cmd,
                    play_local,
                    play_remote,
                    reader,
                    writer,
                )
                .await
                .map_err(Into::<fho::Error>::into)
            }
            SubCommand::Record(_) => {
                let mut stdout = Unblock::new(std::io::stdout());

                let (cancel_proxy, cancel_server) = fidl::endpoints::create_proxy::<
                    fidl_fuchsia_audio_controller::RecordCancelerMarker,
                >()
                .map_err(|e| anyhow::anyhow!("FIDL Error creating canceler proxy: {e}"))?;

                let keypress_waiter = ffx_audio_common::cancel_on_keypress(
                    cancel_proxy,
                    ffx_audio_common::get_stdin_waiter().fuse(),
                );
                let output_result_writer = writer.stderr();

                device_record(
                    self.device_controller,
                    self.record_controller,
                    self.cmd,
                    cancel_server,
                    &mut stdout,
                    output_result_writer,
                    keypress_waiter,
                )
                .await
                .map_err(Into::into)
            }
            SubCommand::Gain(_)
            | SubCommand::Mute(_)
            | SubCommand::Unmute(_)
            | SubCommand::Agc(_) => {
                let direction = self
                    .cmd
                    .device_direction
                    .ok_or(anyhow::anyhow!("Missing device direction argument"))?;
                let id = self.cmd.id.unwrap_or(
                    get_first_device(
                        &self.device_controller,
                        fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
                        direction == DeviceDirection::Input,
                    )
                    .await?
                    .id
                    .ok_or(anyhow::anyhow!("ID missing from default device."))?,
                );
                let mut request_info = DeviceGainStateRequest {
                    device_controller: self.device_controller,
                    device_id: id,
                    device_direction: direction,
                    gain_db: None,
                    agc_enabled: None,
                    muted: None,
                };

                match self.cmd.subcommand {
                    SubCommand::Gain(gain_cmd) => request_info.gain_db = Some(gain_cmd.gain),
                    SubCommand::Mute(..) => request_info.muted = Some(true),
                    SubCommand::Unmute(..) => request_info.muted = Some(false),
                    SubCommand::Agc(agc_command) => {
                        request_info.agc_enabled = Some(agc_command.enable)
                    }
                    _ => {}
                }
                device_set_gain_state(request_info).await.map_err(Into::into)
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceInfoResult {
    pub device_path: String,
    pub manufacturer: Option<String>,
    pub product_name: Option<String>,
    pub current_gain_db: Option<f32>,
    pub mute_state: Option<bool>,
    pub agc_state: Option<bool>,
    pub min_gain: Option<f32>,
    pub max_gain: Option<f32>,
    pub gain_step: Option<f32>,
    pub can_mute: Option<bool>,
    pub can_agc: Option<bool>,
    pub plugged: Option<bool>,
    pub plug_time: Option<i64>,
    pub pd_caps: Option<PdCaps>,
    pub supported_formats: Option<Vec<JsonPcmFormats>>,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum PdCaps {
    Hardwired,
    CanAsyncNotify,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum SampleFormatJson {
    /// Signed Linear Pulse Code Modulation samples at the host endianness.
    PcmSigned,
    /// Unsigned Linear Pulse Code Modulation samples at the host endianness.
    PcmUnsigned,
    /// Floating point samples IEEE-754 encoded.
    PcmFloat,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct JsonPcmFormats {
    /// Vector of possible `SampleFormat`s supported. Required.
    pub sample_formats: Option<Vec<SampleFormatJson>>,
    /// Vector of possible number of bits allocated to hold a sample,
    /// equal or bigger than the actual sample size in `valid_bits_per_sample` in ascending order.
    /// Required.
    pub bytes_per_sample: Option<Vec<u8>>,
    /// Vector of possible number of bits in a sample in ascending order, must be equal or smaller
    /// than `bytes_per_sample` for samples to fit. If smaller, bits are left justified, and any
    /// additional bits will be ignored. Required.
    pub valid_bits_per_sample: Option<Vec<u8>>,
    /// Vector of possible frame rates supported in ascending order. Required.
    pub frame_rates: Option<Vec<u32>>,
}

async fn get_first_device(
    device_controller: &DeviceControlProxy,
    device_type: fidl_fuchsia_hardware_audio::DeviceType,
    is_input: bool,
) -> Result<DeviceSelector> {
    let list_devices_response = device_controller
        .list_devices()
        .await?
        .map_err(|e| anyhow::anyhow!("Could not retrieve available devices. {e}"))?;

    match list_devices_response.devices {
        Some(devices) => devices
            .into_iter()
            .find(|device| {
                device.device_type.is_some_and(|t| t == device_type)
                    && device.is_input.is_some_and(|t| t == is_input)
            })
            .ok_or(anyhow::anyhow!("Could not find any devices with requested type.")),
        None => Err(anyhow::anyhow!("Could not find default device.")),
    }
}

async fn device_info(device_control_proxy: DeviceControlProxy, cmd: DeviceCommand) -> Result<()> {
    let device_direction = cmd
        .device_direction
        .ok_or(anyhow::anyhow!("Device direction not passed to info request."))?;
    let info_cmd = match cmd.subcommand {
        SubCommand::Info(cmd) => cmd,
        _ => panic!("Unreachable."),
    };

    // TODO(https://fxbug.dev/126775): Generalize to DAI & Codec types.
    let (device_selector, is_default_device) = match cmd.id {
        Some(id) => (
            DeviceSelector {
                is_input: Some(device_direction == ffx_audio_device_args::DeviceDirection::Input),
                id: Some(id),
                device_type: Some(fidl_fuchsia_hardware_audio::DeviceType::StreamConfig),
                ..Default::default()
            },
            false,
        ),

        None => (
            get_first_device(
                &device_control_proxy,
                fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
                device_direction == DeviceDirection::Input,
            )
            .await?,
            true,
        ),
    };
    let device_id = device_selector
        .id
        .clone()
        .ok_or(anyhow::anyhow!("Could not get id of requested device."))?;

    let request = DeviceControlGetDeviceInfoRequest {
        device: Some(device_selector.clone()),
        ..Default::default()
    };
    let info = match device_control_proxy.get_device_info(request).await? {
        Ok(value) => value,
        Err(err) => ffx_bail!("Device info failed with error: {}", Status::from_raw(err)),
    };

    let device_type =
        info.device_info.ok_or(anyhow::anyhow!("DeviceInfo missing from response."))?;

    let device_info = match device_type {
        DeviceInfo::StreamConfig(stream_device) => stream_device,
        _ => ffx_bail!("Device info for non StreamConfig devices not implemented"),
    };

    let stream_properties = device_info.stream_properties.clone().ok_or(anyhow::anyhow!(
        "Stream properties field missing for device with id {0}.",
        device_id
    ))?;

    let supported_formats = device_info.supported_formats.clone().ok_or(anyhow::anyhow!(
        "Supported formats field missing for device with id {0}.",
        device_id
    ))?;

    let gain_state = device_info
        .gain_state
        .clone()
        .ok_or(anyhow::anyhow!("Gain state field missing for device with id {0}.", device_id))?;

    let plug_state_info = device_info.plug_state.clone().ok_or(anyhow::anyhow!(
        "Plug state info field missing for device with id {0}.",
        device_id
    ))?;

    let pcm_supported_formats: Vec<PcmSupportedFormats> =
        supported_formats.into_iter().filter_map(|format| format.pcm_supported_formats).collect();

    let print_text_output = || -> Result<(), anyhow::Error> {
        let printable_unique_id: String = match stream_properties.unique_id {
            Some(unique_id) => {
                let formatted: [String; 16] = unique_id
                    .into_iter()
                    .map(|byte| format!("{:02x}", byte))
                    .collect::<Vec<String>>()
                    .try_into()
                    .unwrap_or_default();

                format!("{:}-{:}", formatted[0..7].concat(), formatted[8..15].concat())
            }
            None => format!("Unavaiable"),
        };

        let manufacturer = stream_properties.manufacturer.clone().unwrap_or(format!("Unavailable"));
        let product =
            stream_properties.product.clone().unwrap_or(format!("No product name avaiable"));

        let current_gain_db = match gain_state.gain_db {
            Some(gain_db) => format!("{} db", gain_db),
            None => format!("Gain db not available"),
        };

        let muted = match gain_state.muted {
            Some(muted) => format!("{}", if muted { "muted" } else { "unmuted" },),
            None => format!("Muted not available"),
        };

        let agc = match gain_state.agc_enabled {
            Some(agc) => format!("{}", if agc { "AGC on" } else { "AGC off" }),
            None => format!("AGC not available"),
        };

        // Gain capabilities
        let gain_statement = match stream_properties.min_gain_db {
            Some(min_gain) => match stream_properties.max_gain_db {
                Some(max_gain) => {
                    if min_gain == max_gain {
                        format!("fixed 0 dB gain")
                    } else {
                        format!("gain range [{}, {}]", min_gain, max_gain)
                    }
                }
                None => {
                    format!("Min gain {}. Max gain unavailable.", min_gain)
                }
            },
            None => format!(
                "Min gain unavailable. Max gain {}",
                if stream_properties.max_gain_db.is_some() {
                    format!("{}", stream_properties.max_gain_db.unwrap())
                } else {
                    format!("unavailable.")
                }
            ),
        };

        let gain_step = match stream_properties.gain_step_db {
            Some(gain_step) => {
                if gain_step == 0.0f32 {
                    format!("{} dB (continuous)", gain_step)
                } else {
                    format!("{} in dB steps", gain_step)
                }
            }
            None => format!("Gain step unavailable"),
        };

        let can_mute = match stream_properties.can_mute {
            Some(can_mute) => format!("{}", if can_mute { "can mute" } else { "cannot mute" }),
            None => format!("Can mute unavailable"),
        };

        let can_agc = match stream_properties.can_agc {
            Some(can_agc) => format!("{}", if can_agc { "can agc" } else { "cannot agc" }),
            None => format!("Can agc unavailable"),
        };

        let plug_state = match plug_state_info.plugged {
            Some(plug_state) => format!("{}", if plug_state { "plugged" } else { "unplugged" }),
            None => format!("Unavailable"),
        };

        let plug_time = match plug_state_info.plug_state_time {
            Some(plug_time) => format!("{}", plug_time),
            None => format!("Unavailable"),
        };

        let pd_caps = match stream_properties.plug_detect_capabilities {
            Some(pd_caps) => format!(
                "{}",
                match pd_caps {
                    PlugDetectCapabilities::Hardwired => "Hardwired",
                    PlugDetectCapabilities::CanAsyncNotify => "Can async notify",
                }
            ),
            None => format!("Unavailable"),
        };

        if is_default_device {
            println!("Note: Returning info for first listed device.")
        }
        println!(
            "Info for audio {} at {}",
            if device_selector.is_input.unwrap() { "input" } else { "output" },
            format_utils::path_for_selector(&device_selector)?
        );
        println!("\t Unique ID    : {}", &printable_unique_id);
        println!("\t Manufacturer : {}", manufacturer);
        println!("\t Product      : {}", product);
        println!("\t Current Gain : {} ({}, {})", current_gain_db, muted, agc);
        print!("\t Gain Caps    : ");
        print!("{} {}", gain_statement, gain_step);
        print!("; {} ; {} \n", can_mute, can_agc);

        println!("\t Plug State   : {}", plug_state);
        println!("\t Plug Time    : {}", plug_time);
        println!("\t PD Caps      : {}", pd_caps);

        print!("Number of format sets: {}", pcm_supported_formats.len());
        for format in &pcm_supported_formats {
            println!("\nFormat set");
            print!("\t Number of channels    :");

            let mut has_attributes = false;
            for channel_set in format.clone().channel_sets.unwrap_or(Vec::new()) {
                let num_channels = match channel_set.attributes.clone() {
                    Some(attributes) => {
                        format!("{}", attributes.len())
                    }
                    None => format!("Number of channels unavailable"),
                };

                print!(" {}", num_channels);
                for attribute in channel_set.attributes.unwrap_or(Vec::new()) {
                    if attribute.min_frequency.is_some() {
                        has_attributes = true;
                    }
                    if attribute.max_frequency.is_some() {
                        has_attributes = true;
                    }
                }
            }
            if has_attributes {
                print!(" \n\t Channels attributes     :");
                for channel_set in format.clone().channel_sets.unwrap_or(Vec::new()) {
                    for attribute in channel_set.clone().attributes.unwrap_or(Vec::new()) {
                        match attribute.min_frequency {
                            Some(min_frequency) => println!("  {}", min_frequency),
                            None => {}
                        }
                        match attribute.max_frequency {
                            Some(max_frequency) => println!("  {}", max_frequency),
                            None => {}
                        }
                    }
                    print!(
                        "(min/max Hz for {} channels)",
                        channel_set.attributes.unwrap_or(Vec::new()).len()
                    );
                }
            }

            print!("\n\t Frame rate            :");
            for frame_rate in format.frame_rates.clone().unwrap_or(Vec::new()) {
                print!(" {frame_rate}Hz");
            }

            print!("\n\t Bits per channel      :");
            for bytes_per_sample in format.bytes_per_sample.clone().unwrap_or(Vec::new()) {
                print!(" {}", bytes_per_sample * 8);
            }

            print!("\n\t Valid bits per channel:");
            for valid_bits_per_channel in format.valid_bits_per_sample.clone().unwrap_or(Vec::new())
            {
                println!(" {valid_bits_per_channel}");
            }
        }
        Ok(())
    };

    let print_json = || -> Result<(), anyhow::Error> {
        let device_info = DeviceInfoResult {
            device_path: format_utils::path_for_selector(&device_selector)?,
            manufacturer: stream_properties.manufacturer.clone(),
            product_name: stream_properties.product.clone(),
            current_gain_db: gain_state.gain_db,
            mute_state: gain_state.muted,
            agc_state: gain_state.agc_enabled,
            min_gain: stream_properties.min_gain_db,
            max_gain: stream_properties.max_gain_db,
            gain_step: stream_properties.gain_step_db,
            can_mute: stream_properties.can_mute,
            can_agc: stream_properties.can_agc,
            plugged: plug_state_info.plugged,
            plug_time: plug_state_info.plug_state_time,
            pd_caps: match stream_properties.plug_detect_capabilities {
                Some(PlugDetectCapabilities::CanAsyncNotify) => Some(PdCaps::CanAsyncNotify),
                Some(PlugDetectCapabilities::Hardwired) => Some(PdCaps::Hardwired),
                None => None,
            },
            supported_formats: Some(
                pcm_supported_formats
                    .clone()
                    .into_iter()
                    .map(|format| JsonPcmFormats {
                        sample_formats: format.sample_formats.and_then(|formats| {
                            Some(
                                formats
                                    .into_iter()
                                    .map(|sample_format| match sample_format {
                                        fidl_fuchsia_hardware_audio::SampleFormat::PcmSigned => {
                                            SampleFormatJson::PcmSigned
                                        }
                                        fidl_fuchsia_hardware_audio::SampleFormat::PcmUnsigned => {
                                            SampleFormatJson::PcmUnsigned
                                        }
                                        fidl_fuchsia_hardware_audio::SampleFormat::PcmFloat => {
                                            SampleFormatJson::PcmFloat
                                        }
                                    })
                                    .collect(),
                            )
                        }),
                        bytes_per_sample: format.bytes_per_sample,
                        valid_bits_per_sample: format.valid_bits_per_sample,
                        frame_rates: format.frame_rates,
                    })
                    .collect(),
            ),
        };

        // Serialize it to a JSON string.
        let j = serde_json::to_string(&device_info)?;

        // Print
        println!("{}", j);
        Ok(())
    };

    match info_cmd.output {
        ffx_audio_device_args::InfoOutputFormat::Json => print_json()?,
        ffx_audio_device_args::InfoOutputFormat::Text => print_text_output()?,
    }

    Ok(())
}

async fn device_play(
    device_controller: DeviceControlProxy,
    player_controller: PlayerProxy,
    cmd: DeviceCommand,
    play_local: fidl::Socket,
    play_remote: fidl::Socket,
    input_reader: Box<dyn std::io::Read + std::marker::Send + 'static>,
    // Input generalized to stdin, file, or test buffer.
    mut writer: MachineWriter<DeviceResult>,
) -> Result<(), anyhow::Error> {
    let device_id = match cmd.id {
        Some(id) => Ok(id),
        None => get_first_device(
            &device_controller,
            fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
            false,
        )
        .await
        .and_then(|device| device.id.ok_or(anyhow::anyhow!("Failed to get default device"))),
    }?;
    // Duplicate socket handle so that connection stays alive in real + testing scenarios.
    let remote_socket = play_remote
        .duplicate_handle(fidl::Rights::SAME_RIGHTS)
        .map_err(|e| anyhow::anyhow!("Error duplicating socket: {e}"))?;

    let request = PlayerPlayRequest {
        wav_source: Some(remote_socket),
        destination: Some(fidl_fuchsia_audio_controller::PlayDestination::DeviceRingBuffer(
            fidl_fuchsia_audio_controller::DeviceSelector {
                is_input: Some(false),
                id: Some(device_id),
                device_type: Some(fidl_fuchsia_hardware_audio::DeviceType::StreamConfig),
                ..Default::default()
            },
        )),

        gain_settings: Some(fidl_fuchsia_audio_controller::GainSettings {
            mute: None, // TODO(https://fxbug.dev/121211)
            gain: None, // TODO(https://fxbug.dev/121211)
            ..Default::default()
        }),
        ..Default::default()
    };

    let result =
        ffx_audio_common::play(request, player_controller, play_local, input_reader).await?;
    let bytes_processed = result.bytes_processed;
    let value = DeviceResult::Play(result);

    let _ = writer
        .machine_or_else(&value, || {
            format!("Successfully processed all audio data. Bytes processed: {:?}", {
                bytes_processed
                    .map(|bytes| bytes.to_string())
                    .unwrap_or_else(|| format!("Unavailable"))
            })
        })
        .map_err(Into::<anyhow::Error>::into)?;
    Ok(())
}

async fn device_record<W, E>(
    daemon_proxy: DeviceControlProxy,
    controller: RecorderProxy,
    cmd: DeviceCommand,
    cancel_server: ServerEnd<RecordCancelerMarker>,
    mut output_writer: W,
    mut output_error_writer: E,
    keypress_waiter: impl futures::Future<Output = Result<(), std::io::Error>>,
) -> Result<()>
where
    W: AsyncWrite + std::marker::Unpin,
    E: std::io::Write,
{
    let device_id = match cmd.id {
        Some(id) => Ok(id),
        None => get_first_device(
            &daemon_proxy,
            fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
            true,
        )
        .await
        .and_then(|device| device.id.ok_or(anyhow::anyhow!("Failed to get default device."))),
    }?;

    let record_command = match cmd.subcommand {
        SubCommand::Record(record_command) => record_command,
        _ => ffx_bail!("Unreachable"),
    };

    let (record_remote, record_local) = fidl::Socket::create_datagram();

    let request = RecorderRecordRequest {
        source: Some(RecordSource::DeviceRingBuffer(
            fidl_fuchsia_audio_controller::DeviceSelector {
                is_input: Some(true),
                id: Some(device_id),
                device_type: Some(fidl_fuchsia_hardware_audio::DeviceType::StreamConfig),
                ..Default::default()
            },
        )),

        stream_type: Some(AudioStreamType::from(&record_command.format)),
        duration: record_command.duration.map(|duration| duration.as_nanos() as i64),
        canceler: Some(cancel_server),
        wav_data: Some(record_remote),
        ..Default::default()
    };

    let result = ffx_audio_common::record(
        controller,
        request,
        record_local,
        &mut output_writer,
        keypress_waiter,
    )
    .await;

    let message = ffx_audio_common::format_record_result(result);

    writeln!(output_error_writer, "{}", message)
        .map_err(|e| anyhow::anyhow!("Writing result failed with error {e}."))?;
    Ok(())
}

struct DeviceGainStateRequest {
    device_controller: DeviceControlProxy,
    device_id: String,
    device_direction: DeviceDirection,
    muted: Option<bool>,
    gain_db: Option<f32>,
    agc_enabled: Option<bool>,
}

async fn device_set_gain_state(request: DeviceGainStateRequest) -> Result<()> {
    let dev_selector = DeviceSelector {
        is_input: Some(DeviceDirection::Input == request.device_direction),
        id: Some(request.device_id),
        ..Default::default()
    };

    let gain_state = fidl_fuchsia_hardware_audio::GainState {
        muted: request.muted,
        gain_db: request.gain_db,
        agc_enabled: request.agc_enabled,
        ..Default::default()
    };

    request
        .device_controller
        .device_set_gain_state(DeviceControlDeviceSetGainStateRequest {
            device: Some(dev_selector),
            gain_state: Some(gain_state),
            ..Default::default()
        })
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("Error setting gain state. {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_audio_common::tests::SINE_WAV;
    use ffx_audio_device_args::{
        DeviceCommand, DeviceDirection, DevicePlayCommand, DeviceRecordCommand,
    };
    use ffx_core::macro_deps::futures::AsyncWriteExt;
    use ffx_writer::TestBuffer;
    use ffx_writer::{SimpleWriter, TestBuffers};
    use fidl::HandleBased;
    use format_utils::Format;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[fuchsia::test]
    pub async fn test_play_success() -> Result<(), fho::Error> {
        let audio_daemon = ffx_audio_common::tests::fake_audio_daemon();
        let audio_player = ffx_audio_common::tests::fake_audio_player();

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<DeviceResult> = MachineWriter::new_test(None, &test_buffers);

        let command = DeviceCommand {
            subcommand: ffx_audio_device_args::SubCommand::Play(DevicePlayCommand { file: None }),
            id: Some("abc123".to_string()),
            device_direction: Some(DeviceDirection::Output),
        };

        let (play_remote, play_local) = fidl::Socket::create_datagram();
        let mut async_play_local = fidl::AsyncSocket::from_socket(
            play_local.duplicate_handle(fidl::Rights::SAME_RIGHTS).unwrap(),
        );

        async_play_local.write_all(ffx_audio_common::tests::WAV_HEADER_EXT).await.unwrap();

        let result = device_play(
            audio_daemon.clone(),
            audio_player.clone(),
            command,
            play_local,
            play_remote,
            Box::new(&ffx_audio_common::tests::WAV_HEADER_EXT[..]),
            writer,
        )
        .await;

        result.unwrap();
        let expected_output =
            format!("Successfully processed all audio data. Bytes processed: \"1\"\n");
        let stdout = test_buffers.into_stdout_str();
        assert_eq!(stdout, expected_output);

        // Test reading from a file.
        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<DeviceResult> = MachineWriter::new_test(None, &test_buffers);

        let test_dir = TempDir::new().unwrap();
        let test_dir_path = test_dir.path().to_path_buf();
        let test_wav_path = test_dir_path.join("sine.wav");
        let wav_path = test_wav_path
            .clone()
            .into_os_string()
            .into_string()
            .map_err(|_e| anyhow::anyhow!("Error turning path into string"))?;

        // Create valid WAV file.
        fs::File::create(&test_wav_path)
            .unwrap()
            .write_all(ffx_audio_common::tests::SINE_WAV)
            .unwrap();
        fs::set_permissions(&test_wav_path, fs::Permissions::from_mode(0o770)).unwrap();

        let file_reader = std::fs::File::open(&test_wav_path)
            .map_err(|e| anyhow::anyhow!("Error trying to open file \"{}\": {e}", wav_path))?;

        let file_command = DeviceCommand {
            subcommand: ffx_audio_device_args::SubCommand::Play(DevicePlayCommand {
                file: Some(wav_path),
            }),
            id: Some("abc123".to_string()),
            device_direction: Some(DeviceDirection::Input),
        };
        let (play_remote, play_local) = fidl::Socket::create_datagram();
        let result = device_play(
            audio_daemon.clone(),
            audio_player.clone(),
            file_command,
            play_local,
            play_remote,
            Box::new(file_reader),
            writer,
        )
        .await;
        result.unwrap();
        let expected_output =
            format!("Successfully processed all audio data. Bytes processed: \"1\"\n");
        let stdout = test_buffers.into_stdout_str();
        assert_eq!(stdout, expected_output);
        Ok(())
    }

    #[fuchsia::test]
    pub async fn test_record_no_cancel() -> Result<(), fho::Error> {
        // Test without sending a cancel message. Still set up the canceling proxy and server,
        // but never send the message from proxy to daemon to cancel. Test daemon should
        // exit after duration (real daemon exits after sending all duration amount of packets).
        let controller = ffx_audio_common::tests::fake_audio_recorder();
        let audio_daemon = ffx_audio_common::tests::fake_audio_daemon();
        let test_buffers = TestBuffers::default();
        let mut result_writer: SimpleWriter = SimpleWriter::new_test(&test_buffers);

        let command = DeviceCommand {
            subcommand: ffx_audio_device_args::SubCommand::Record(DeviceRecordCommand {
                duration: Some(std::time::Duration::from_nanos(500)),
                format: Format {
                    sample_type: fidl_fuchsia_media::AudioSampleFormat::Unsigned8,
                    frames_per_second: 48000,
                    channels: 1,
                },
            }),
            id: Some("abc123".to_string()),
            device_direction: Some(DeviceDirection::Input),
        };

        let (cancel_proxy, cancel_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_audio_controller::RecordCancelerMarker>()
                .unwrap();

        let test_stdout = TestBuffer::default();

        // Pass a future that will never complete as an input waiter.
        let keypress_waiter =
            ffx_audio_common::cancel_on_keypress(cancel_proxy, futures::future::pending().fuse());

        let _res = device_record(
            audio_daemon,
            controller,
            command,
            cancel_server,
            test_stdout.clone(),
            result_writer.stderr(),
            keypress_waiter,
        )
        .await?;

        let expected_result_output =
            format!("Successfully recorded 123 bytes of audio. \nPackets processed: 123 \nLate wakeups: Unavailable\n");
        let stderr = test_buffers.into_stderr_str();
        assert_eq!(stderr, expected_result_output);

        let stdout = test_stdout.into_inner();
        let expected_wav_output = Vec::from(SINE_WAV);
        assert_eq!(stdout, expected_wav_output);
        Ok(())
    }

    #[fuchsia::test]
    pub async fn test_record_immediate_cancel() -> Result<(), fho::Error> {
        let controller = ffx_audio_common::tests::fake_audio_recorder();
        let audio_daemon = ffx_audio_common::tests::fake_audio_daemon();
        let test_buffers = TestBuffers::default();
        let mut result_writer: SimpleWriter = SimpleWriter::new_test(&test_buffers);

        let command = DeviceCommand {
            subcommand: ffx_audio_device_args::SubCommand::Record(DeviceRecordCommand {
                duration: None,
                format: Format {
                    sample_type: fidl_fuchsia_media::AudioSampleFormat::Unsigned8,
                    frames_per_second: 48000,
                    channels: 1,
                },
            }),
            id: Some("abc123".to_string()),
            device_direction: Some(DeviceDirection::Input),
        };

        let (cancel_proxy, cancel_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_audio_controller::RecordCancelerMarker>()
                .unwrap();

        let test_stdout = TestBuffer::default();

        // Test canceler signaling. Not concerned with how much data gets back through socket.
        // Test failing is never finishing execution before timeout.
        let keypress_waiter =
            ffx_audio_common::cancel_on_keypress(cancel_proxy, futures::future::ready(Ok(())));

        let _res = device_record(
            audio_daemon,
            controller,
            command,
            cancel_server,
            test_stdout.clone(),
            result_writer.stderr(),
            keypress_waiter,
        )
        .await?;
        Ok(())
    }
}
