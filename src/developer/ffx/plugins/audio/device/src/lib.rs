// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    async_trait::async_trait,
    blocking::Unblock,
    errors::ffx_bail,
    ffx_audio_device_args::{DeviceCommand, DeviceDirection, SubCommand},
    fho::{selector, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_audio_ffxdaemon::{
        AudioDaemonDeviceInfoRequest, AudioDaemonDeviceSetGainStateRequest, AudioDaemonPlayRequest,
        AudioDaemonProxy, AudioDaemonRecordRequest, DeviceSelector, RecordLocation,
    },
    fidl_fuchsia_hardware_audio::{PcmSupportedFormats, PlugDetectCapabilities},
    fidl_fuchsia_media::AudioStreamType,
    fuchsia_zircon_status::Status,
    futures,
    serde::{Deserialize, Serialize},
};

#[derive(FfxTool)]
pub struct DeviceTool {
    #[command]
    cmd: DeviceCommand,
    #[with(selector("core/audio_ffx_daemon:expose:fuchsia.audio.ffxdaemon.AudioDaemon"))]
    audio_proxy: AudioDaemonProxy,
}

fho::embedded_plugin!(DeviceTool);
#[async_trait(?Send)]
impl FfxMain for DeviceTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        match self.cmd.subcommand {
            SubCommand::Info(_) => {
                device_info(self.audio_proxy, self.cmd).await.map_err(Into::into)
            }
            SubCommand::Play(_) => {
                device_play(self.audio_proxy, self.cmd).await.map_err(Into::into)
            }
            SubCommand::Record(_) => {
                device_record(self.audio_proxy, self.cmd).await.map_err(Into::into)
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
                        &self.audio_proxy,
                        fidl_fuchsia_virtualaudio::DeviceType::StreamConfig,
                        direction == DeviceDirection::Input,
                    )
                    .await?
                    .id
                    .ok_or(anyhow::anyhow!("ID missing from default device."))?,
                );
                let mut request_info = DeviceGainStateRequest {
                    audio_proxy: self.audio_proxy,
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
    audio_proxy: &AudioDaemonProxy,
    device_type: fidl_fuchsia_virtualaudio::DeviceType,
    is_input: bool,
) -> Result<DeviceSelector> {
    let list_devices_response = audio_proxy
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

async fn device_info(audio_proxy: AudioDaemonProxy, cmd: DeviceCommand) -> Result<()> {
    let device_direction = cmd
        .device_direction
        .ok_or(anyhow::anyhow!("Device direction not passed to info request."))?;
    let info_cmd = match cmd.subcommand {
        SubCommand::Info(cmd) => cmd,
        _ => panic!("Unreachable."),
    };

    // TODO(fxbug.dev/126775): Generalize to DAI & Codec types.
    let (device_selector, is_default_device) = match cmd.id {
        Some(id) => (
            DeviceSelector {
                is_input: Some(device_direction == ffx_audio_device_args::DeviceDirection::Input),
                id: Some(id),
                device_type: Some(fidl_fuchsia_virtualaudio::DeviceType::StreamConfig),
                ..Default::default()
            },
            false,
        ),

        None => (
            get_first_device(
                &audio_proxy,
                fidl_fuchsia_virtualaudio::DeviceType::StreamConfig,
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

    let request = AudioDaemonDeviceInfoRequest {
        device: Some(device_selector.clone()),
        ..Default::default()
    };
    let info = match audio_proxy.device_info(request).await? {
        Ok(value) => value,
        Err(err) => ffx_bail!("Device info failed with error: {}", Status::from_raw(err)),
    };

    let device_info =
        info.device_info.ok_or(anyhow::anyhow!("DeviceInfo missing from response."))?;

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
            Some(unique_id) => std::str::from_utf8(&unique_id)?.to_owned(),
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

async fn device_play(audio_proxy: AudioDaemonProxy, cmd: DeviceCommand) -> Result<()> {
    let (play_remote, play_local) = fidl::Socket::create_datagram();

    let device_id = match cmd.id {
        Some(id) => Ok(id),
        None => get_first_device(
            &audio_proxy,
            fidl_fuchsia_virtualaudio::DeviceType::StreamConfig,
            false,
        )
        .await
        .and_then(|device| device.id.ok_or(anyhow::anyhow!("Failed to get default device"))),
    }?;
    let request = AudioDaemonPlayRequest {
        socket: Some(play_remote),
        location: Some(fidl_fuchsia_audio_ffxdaemon::PlayLocation::RingBuffer(
            fidl_fuchsia_audio_ffxdaemon::DeviceSelector {
                is_input: Some(false),
                id: Some(device_id),
                device_type: Some(fidl_fuchsia_virtualaudio::DeviceType::StreamConfig),
                ..Default::default()
            },
        )),

        gain_settings: Some(fidl_fuchsia_audio_ffxdaemon::GainSettings {
            mute: None, // TODO(fxbug.dev/121211)
            gain: None, // TODO(fxbug.dev/121211)
            ..Default::default()
        }),
        ..Default::default()
    };

    ffx_audio_common::play(request, audio_proxy, play_local).await?;
    Ok(())
}

async fn device_record(audio_proxy: AudioDaemonProxy, cmd: DeviceCommand) -> Result<()> {
    let device_id = match cmd.id {
        Some(id) => Ok(id),
        None => get_first_device(
            &audio_proxy,
            fidl_fuchsia_virtualaudio::DeviceType::StreamConfig,
            true,
        )
        .await
        .and_then(|device| device.id.ok_or(anyhow::anyhow!("Failed to get default device."))),
    }?;

    let record_command = match cmd.subcommand {
        SubCommand::Record(record_command) => record_command,
        _ => ffx_bail!("Unreachable"),
    };

    let (cancel_client, cancel_server) = fidl::endpoints::create_endpoints::<
        fidl_fuchsia_audio_ffxdaemon::AudioDaemonCancelerMarker,
    >();

    let request = AudioDaemonRecordRequest {
        location: Some(RecordLocation::RingBuffer(fidl_fuchsia_audio_ffxdaemon::DeviceSelector {
            is_input: Some(true),
            id: Some(device_id),
            device_type: Some(fidl_fuchsia_virtualaudio::DeviceType::StreamConfig),
            ..Default::default()
        })),

        stream_type: Some(AudioStreamType::from(&record_command.format)),
        duration: record_command.duration.map(|duration| duration.as_nanos() as i64),
        canceler: Some(cancel_server),
        ..Default::default()
    };

    let (stdout_sock, stderr_sock) = match audio_proxy.record(request).await? {
        Ok(value) => (
            value.stdout.ok_or(anyhow::anyhow!("No stdout socket"))?,
            value.stderr.ok_or(anyhow::anyhow!("No stderr socket"))?,
        ),
        Err(err) => ffx_bail!("Record failed with err: {}", err),
    };

    let mut stdout = Unblock::new(std::io::stdout());
    let mut stderr = Unblock::new(std::io::stderr());

    futures::future::try_join3(
        futures::io::copy(fidl::AsyncSocket::from_socket(stdout_sock)?, &mut stdout),
        futures::io::copy(fidl::AsyncSocket::from_socket(stderr_sock)?, &mut stderr),
        ffx_audio_common::wait_for_keypress(cancel_client),
    )
    .await
    .map(|_| ())
    .map_err(|e| anyhow::anyhow!("Error copying data from socket. {}", e))
}

struct DeviceGainStateRequest {
    audio_proxy: AudioDaemonProxy,
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
        .audio_proxy
        .device_set_gain_state(AudioDaemonDeviceSetGainStateRequest {
            device: Some(dev_selector),
            gain_state: Some(gain_state),
            ..Default::default()
        })
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("Error setting gain state. {e}"))
}
