// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    blocking::Unblock,
    errors::ffx_bail,
    ffx_audio_common::DeviceResult,
    ffx_audio_device_args::{DeviceCommand, DeviceDirection, SubCommand},
    ffx_command::{bug, user_error},
    fho::{moniker, FfxContext, FfxMain, FfxTool, MachineWriter, ToolIO},
    fidl::{endpoints::ServerEnd, HandleBased},
    fidl_fuchsia_audio_controller::{
        DeviceControlDeviceSetGainStateRequest, DeviceControlGetDeviceInfoRequest,
        DeviceControlProxy, DeviceSelector, PlayerPlayRequest, PlayerProxy, RecordCancelerMarker,
        RecordSource, RecorderProxy, RecorderRecordRequest,
    },
    fidl_fuchsia_media::AudioStreamType,
    fuchsia_zircon_status::Status,
    futures::AsyncWrite,
    futures::FutureExt,
    std::io::Read,
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
            SubCommand::Info(_) => device_info(self.device_controller, self.cmd, writer).await,
            SubCommand::Play(play_command) => {
                let (play_remote, play_local) = fidl::Socket::create_datagram();
                let reader: Box<dyn Read + Send + 'static> = match &play_command.file {
                    Some(input_file_path) => {
                        let file =
                            std::fs::File::open(&input_file_path).with_user_message(|| {
                                format!("Failed to open file \"{input_file_path}\"")
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
            }
            SubCommand::Record(_) => {
                let mut stdout = Unblock::new(std::io::stdout());

                let (cancel_proxy, cancel_server) =
                    fidl::endpoints::create_proxy::<RecordCancelerMarker>().bug()?;

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
            }
            SubCommand::Gain(_)
            | SubCommand::Mute(_)
            | SubCommand::Unmute(_)
            | SubCommand::Agc(_) => {
                let device_direction = self.cmd.device_direction;

                let device_id = match self.cmd.id {
                    Some(id) => id,
                    None => {
                        let is_input = device_direction
                            .as_ref()
                            .and_then(|direction| Some(direction == &DeviceDirection::Input));
                        let device = get_first_device(
                            &self.device_controller,
                            fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
                            is_input,
                        )
                        .await?;
                        device.id.ok_or_else(|| bug!("Device ID missing from response"))?
                    }
                };

                let mut request_info = DeviceGainStateRequest {
                    device_controller: self.device_controller,
                    device_id,
                    device_direction,
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
                device_set_gain_state(request_info).await
            }
        }
    }
}

async fn get_first_device(
    device_controller: &DeviceControlProxy,
    device_type: fidl_fuchsia_hardware_audio::DeviceType,
    is_input: Option<bool>,
) -> fho::Result<DeviceSelector> {
    let list_devices_response = device_controller
        .list_devices()
        .await
        .bug_context("Failed to call DeviceControl.ListDevices")?
        .map_err(|status| Status::from_raw(status))
        .bug_context("Could not retrieve available devices")?;

    let devices =
        list_devices_response.devices.ok_or_else(|| bug!("Devices missing from response"))?;
    devices
        .into_iter()
        .find(|device| {
            device.device_type.is_some_and(|t| t == device_type) && device.is_input == is_input
        })
        .ok_or_else(|| user_error!("Could not find any devices with requested type"))
}

async fn device_info(
    device_control_proxy: DeviceControlProxy,
    cmd: DeviceCommand,
    mut writer: MachineWriter<DeviceResult>,
) -> fho::Result<()> {
    let device_direction = cmd.device_direction;
    let is_input = device_direction
        .and_then(|direction| Some(direction == ffx_audio_device_args::DeviceDirection::Input));

    let device_selector = match cmd.id {
        Some(id) => DeviceSelector {
            is_input,
            id: Some(id),
            device_type: Some(cmd.device_type),
            ..Default::default()
        },
        None => get_first_device(&device_control_proxy, cmd.device_type, is_input).await?,
    };

    let request = DeviceControlGetDeviceInfoRequest {
        device: Some(device_selector.clone()),
        ..Default::default()
    };
    let info = device_control_proxy
        .get_device_info(request)
        .await
        .bug_context("Failed to call DeviceControl.GetDeviceInfo")?
        .map_err(|status| Status::from_raw(status))
        .bug_context("Failed to get device info")?;

    let device_info = info.device_info.ok_or_else(|| bug!("DeviceInfo missing from response."))?;
    let device_info_result =
        ffx_audio_common::device_info::DeviceInfoResult::from((device_info, &device_selector));
    let device_result = DeviceResult::Info(device_info_result.clone());

    writer.machine_or_else(&device_result, || format!("{}", device_info_result))?;

    Ok(())
}

async fn device_play(
    device_controller: DeviceControlProxy,
    player_controller: PlayerProxy,
    cmd: DeviceCommand,
    play_local: fidl::Socket,
    play_remote: fidl::Socket,
    input_reader: Box<dyn Read + Send + 'static>,
    // Input generalized to stdin, file, or test buffer.
    mut writer: MachineWriter<DeviceResult>,
) -> fho::Result<()> {
    let device_id = match cmd.id {
        Some(id) => id,
        None => {
            let device = get_first_device(
                &device_controller,
                fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
                Some(false),
            )
            .await?;
            device.id.ok_or_else(|| bug!("Device ID missing from response"))?
        }
    };

    // Duplicate socket handle so that connection stays alive in real + testing scenarios.
    let remote_socket = play_remote
        .duplicate_handle(fidl::Rights::SAME_RIGHTS)
        .bug_context("Error duplicating socket")?;

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
            mute: None, // TODO(https://fxbug.dev/42072218)
            gain: None, // TODO(https://fxbug.dev/42072218)
            ..Default::default()
        }),
        ..Default::default()
    };

    let result =
        ffx_audio_common::play(request, player_controller, play_local, input_reader).await?;
    let bytes_processed = result.bytes_processed;
    let value = DeviceResult::Play(result);

    writer.machine_or_else(&value, || {
        format!("Successfully processed all audio data. Bytes processed: {:?}", {
            bytes_processed
                .map(|bytes| bytes.to_string())
                .unwrap_or_else(|| "Unavailable".to_string())
        })
    })?;

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
) -> fho::Result<()>
where
    W: AsyncWrite + std::marker::Unpin,
    E: std::io::Write,
{
    let device_id = match cmd.id {
        Some(id) => id,
        None => {
            let device = get_first_device(
                &daemon_proxy,
                fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
                Some(true),
            )
            .await?;
            device.id.ok_or_else(|| bug!("Device ID missing from response"))?
        }
    };

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

    writeln!(output_error_writer, "{}", message).bug_context("Failed to write result")?;

    Ok(())
}

struct DeviceGainStateRequest {
    device_controller: DeviceControlProxy,
    device_id: String,
    device_direction: Option<DeviceDirection>,
    muted: Option<bool>,
    gain_db: Option<f32>,
    agc_enabled: Option<bool>,
}

async fn device_set_gain_state(request: DeviceGainStateRequest) -> fho::Result<()> {
    let dev_selector = DeviceSelector {
        is_input: request
            .device_direction
            .and_then(|direction| Some(direction == DeviceDirection::Input)),
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
        .bug_context("Failed to call DeviceControl.DeviceSetGainState")?
        .map_err(|status| Status::from_raw(status))
        .bug_context("Failed to set gain state")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_audio_common::tests::SINE_WAV;
    use ffx_audio_device_args::{DevicePlayCommand, DeviceRecordCommand, InfoCommand};
    use ffx_core::macro_deps::futures::AsyncWriteExt;
    use ffx_writer::{SimpleWriter, TestBuffer, TestBuffers};
    use fuchsia_audio::Format;
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
            device_type: fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
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
        let wav_path = test_wav_path.clone().into_os_string().into_string().unwrap();

        // Create valid WAV file.
        fs::File::create(&test_wav_path)
            .unwrap()
            .write_all(ffx_audio_common::tests::SINE_WAV)
            .unwrap();
        fs::set_permissions(&test_wav_path, fs::Permissions::from_mode(0o770)).unwrap();

        let file_reader = std::fs::File::open(&test_wav_path)
            .with_bug_context(|| format!("Error trying to open file \"{}\"", wav_path))?;

        let file_command = DeviceCommand {
            subcommand: ffx_audio_device_args::SubCommand::Play(DevicePlayCommand {
                file: Some(wav_path),
            }),
            id: Some("abc123".to_string()),
            device_direction: Some(DeviceDirection::Input),
            device_type: fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
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
            device_type: fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
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
            device_type: fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
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

    #[fuchsia::test]
    pub async fn test_stream_config_info() -> Result<(), fho::Error> {
        let audio_daemon = ffx_audio_common::tests::fake_audio_daemon();
        let command = DeviceCommand {
            subcommand: ffx_audio_device_args::SubCommand::Info(InfoCommand {}),
            id: Some(format!("abc123")),
            device_direction: Some(DeviceDirection::Input),
            device_type: fidl_fuchsia_hardware_audio::DeviceType::StreamConfig,
        };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<DeviceResult> =
            MachineWriter::new_test(Some(ffx_writer::Format::Json), &test_buffers);
        let result = device_info(audio_daemon, command, writer).await;
        result.unwrap();

        let stdout = test_buffers.into_stdout_str();
        let stdout_expected = format!(
            "{{\"Info\":{{\"device_path\":\"/dev/class/audio-input/abc123\",\
            \"manufacturer\":\"Spacely Sprockets\",\"product_name\":\"Test Microphone\",\
            \"current_gain_db\":null,\"mute_state\":null,\"agc_state\":null,\"min_gain\":-32.0,\
            \"max_gain\":60.0,\"gain_step\":0.5,\"can_mute\":true,\"can_agc\":true,\
            \"plugged\":null,\"plug_time\":null,\"pd_caps\":null,\"supported_formats\":null,\
            \"unique_id\":\"00010203040506-08090a0b0c0d0e\",\"clock_domain\":null}}}}\n"
        );

        assert_eq!(stdout, stdout_expected);

        Ok(())
    }

    #[fuchsia::test]
    pub async fn test_composite_info() -> Result<(), fho::Error> {
        let audio_daemon = ffx_audio_common::tests::fake_audio_daemon();
        let command = DeviceCommand {
            subcommand: ffx_audio_device_args::SubCommand::Info(InfoCommand {}),
            id: Some(format!("abc123")),
            device_direction: None,
            device_type: fidl_fuchsia_hardware_audio::DeviceType::Composite,
        };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<DeviceResult> =
            MachineWriter::new_test(Some(ffx_writer::Format::Json), &test_buffers);
        let result = device_info(audio_daemon, command, writer).await;
        result.unwrap();

        let stdout = test_buffers.into_stdout_str();
        let stdout_expected = format!(
            "{{\"Info\":{{\"device_path\":\"/dev/class/audio-composite/abc123\",\
            \"manufacturer\":null,\"product_name\":null,\
            \"current_gain_db\":null,\"mute_state\":null,\"agc_state\":null,\"min_gain\":null,\
            \"max_gain\":null,\"gain_step\":null,\"can_mute\":null,\"can_agc\":null,\
            \"plugged\":null,\"plug_time\":null,\"pd_caps\":null,\"supported_formats\":null,\
            \"unique_id\":null,\"clock_domain\":0}}}}\n"
        );

        assert_eq!(stdout, stdout_expected);

        Ok(())
    }
}
