// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::list::DeviceQuery,
    async_trait::async_trait,
    blocking::Unblock,
    ffx_audio_common::ffxtool::exposed_dir,
    ffx_audio_device_args::{DeviceCommand, DeviceRecordCommand, SubCommand},
    ffx_command::{bug, user_error},
    fho::{moniker, FfxContext, FfxMain, FfxTool, MachineWriter, ToolIO},
    fidl::{endpoints::ServerEnd, HandleBased},
    fidl_fuchsia_audio_controller::{
        DeviceControlDeviceSetGainStateRequest, DeviceControlGetDeviceInfoRequest,
        DeviceControlProxy, PlayerPlayRequest, PlayerProxy, RecordCancelerMarker, RecordSource,
        RecorderProxy, RecorderRecordRequest,
    },
    fidl_fuchsia_io as fio,
    fidl_fuchsia_media::AudioStreamType,
    fuchsia_audio::device::Selector,
    fuchsia_zircon_status::Status,
    futures::{AsyncWrite, FutureExt},
    serde::Serialize,
    std::io::Read,
};

pub mod list;

#[derive(Debug, Serialize)]
pub enum DeviceResult {
    Play(ffx_audio_common::PlayResult),
    Record(ffx_audio_common::RecordResult),
    Info(ffx_audio_common::device_info::DeviceInfoResult),
    List(list::ListResult),
}

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
    #[with(exposed_dir("/bootstrap/devfs", "dev-class"))]
    dev_class: fio::DirectoryProxy,
}

fho::embedded_plugin!(DeviceTool);
#[async_trait(?Send)]
impl FfxMain for DeviceTool {
    type Writer = MachineWriter<DeviceResult>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let mut selectors = {
            let query = DeviceQuery::try_from(&self.cmd)
                .map_err(|msg| user_error!("Invalid device query: {msg}"))?;
            list::get_devices(&self.dev_class)
                .await
                .bug_context("Failed to get devices")?
                .into_iter()
                .filter(move |selector| query.matches(selector))
        };

        // The list command consumes all device selectors to print them.
        if let SubCommand::List(_) = self.cmd.subcommand {
            return device_list(selectors.collect::<Vec<_>>(), writer);
        }

        // For all other commands, pick the first matching device.
        let selector =
            selectors.next().ok_or_else(|| user_error!("Could not find a matching device"))?;

        match self.cmd.subcommand {
            SubCommand::List(_) => unreachable!(),
            SubCommand::Info(_) => device_info(self.device_controller, selector, writer).await,
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

                device_play(self.play_controller, selector, play_local, play_remote, reader, writer)
                    .await
            }
            SubCommand::Record(record_command) => {
                let mut stdout = Unblock::new(std::io::stdout());

                let (cancel_proxy, cancel_server) =
                    fidl::endpoints::create_proxy::<RecordCancelerMarker>().bug()?;

                let keypress_waiter = ffx_audio_common::cancel_on_keypress(
                    cancel_proxy,
                    ffx_audio_common::get_stdin_waiter().fuse(),
                );
                let output_result_writer = writer.stderr();

                device_record(
                    self.record_controller,
                    selector,
                    record_command,
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
                let mut gain_state = fidl_fuchsia_hardware_audio::GainState::default();

                match self.cmd.subcommand {
                    SubCommand::Gain(gain_cmd) => gain_state.gain_db = Some(gain_cmd.gain),
                    SubCommand::Mute(..) => gain_state.muted = Some(true),
                    SubCommand::Unmute(..) => gain_state.muted = Some(false),
                    SubCommand::Agc(agc_command) => {
                        gain_state.agc_enabled = Some(agc_command.enable)
                    }
                    _ => {}
                }

                device_set_gain_state(self.device_controller, selector, gain_state).await
            }
        }
    }
}

async fn device_info(
    device_control: DeviceControlProxy,
    selector: Selector,
    mut writer: MachineWriter<DeviceResult>,
) -> fho::Result<()> {
    let info = device_control
        .get_device_info(DeviceControlGetDeviceInfoRequest {
            device: Some(selector.clone().into()),
            ..Default::default()
        })
        .await
        .bug_context("Failed to call DeviceControl.GetDeviceInfo")?
        .map_err(|status| Status::from_raw(status))
        .bug_context("Failed to get device info")?;

    let device_info = info.device_info.ok_or_else(|| bug!("DeviceInfo missing from response."))?;
    let device_info_result =
        ffx_audio_common::device_info::DeviceInfoResult::from((device_info, selector));
    let device_result = DeviceResult::Info(device_info_result.clone());

    writer.machine_or_else(&device_result, || format!("{}", device_info_result))?;

    Ok(())
}

async fn device_play(
    player_controller: PlayerProxy,
    selector: Selector,
    play_local: fidl::Socket,
    play_remote: fidl::Socket,
    input_reader: Box<dyn Read + Send + 'static>,
    // Input generalized to stdin, file, or test buffer.
    mut writer: MachineWriter<DeviceResult>,
) -> fho::Result<()> {
    // Duplicate socket handle so that connection stays alive in real + testing scenarios.
    let remote_socket = play_remote
        .duplicate_handle(fidl::Rights::SAME_RIGHTS)
        .bug_context("Error duplicating socket")?;

    let request = PlayerPlayRequest {
        wav_source: Some(remote_socket),
        destination: Some(fidl_fuchsia_audio_controller::PlayDestination::DeviceRingBuffer(
            selector.into(),
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
    recorder: RecorderProxy,
    selector: Selector,
    record_command: DeviceRecordCommand,
    cancel_server: ServerEnd<RecordCancelerMarker>,
    mut output_writer: W,
    mut output_error_writer: E,
    keypress_waiter: impl futures::Future<Output = Result<(), std::io::Error>>,
) -> fho::Result<()>
where
    W: AsyncWrite + std::marker::Unpin,
    E: std::io::Write,
{
    let (record_remote, record_local) = fidl::Socket::create_datagram();

    let request = RecorderRecordRequest {
        source: Some(RecordSource::DeviceRingBuffer(selector.into())),
        stream_type: Some(AudioStreamType::from(&record_command.format)),
        duration: record_command.duration.map(|duration| duration.as_nanos() as i64),
        canceler: Some(cancel_server),
        wav_data: Some(record_remote),
        ..Default::default()
    };

    let result = ffx_audio_common::record(
        recorder,
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

async fn device_set_gain_state(
    device_control: DeviceControlProxy,
    selector: Selector,
    gain_state: fidl_fuchsia_hardware_audio::GainState,
) -> fho::Result<()> {
    device_control
        .device_set_gain_state(DeviceControlDeviceSetGainStateRequest {
            device: Some(selector.into()),
            gain_state: Some(gain_state),
            ..Default::default()
        })
        .await
        .bug_context("Failed to call DeviceControl.DeviceSetGainState")?
        .map_err(|status| Status::from_raw(status))
        .bug_context("Failed to set gain state")
}

fn device_list(
    selectors: Vec<Selector>,
    mut writer: MachineWriter<DeviceResult>,
) -> fho::Result<()> {
    let list_result = list::ListResult::from(selectors);
    let result = DeviceResult::List(list_result.clone());
    writer
        .machine_or_else(&result, || format!("{}", list_result))
        .bug_context("Failed to write result")
}

// TODO(https://fxbug.dev/330584540): Remove this method and make all device
// machine output use #[serde(untagged)].
pub fn device_list_untagged(
    selectors: Vec<Selector>,
    mut writer: MachineWriter<list::ListResult>,
) -> fho::Result<()> {
    let list_result = list::ListResult::from(selectors);
    writer
        .machine_or_else(&list_result, || format!("{}", &list_result))
        .bug_context("Failed to write result")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_audio_common::tests::SINE_WAV;
    use ffx_core::macro_deps::futures::AsyncWriteExt;
    use ffx_writer::{SimpleWriter, TestBuffer, TestBuffers};
    use fidl_fuchsia_audio_controller as fac;
    use fidl_fuchsia_audio_device as fadevice;
    use fuchsia_audio::Format;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[fuchsia::test]
    pub async fn test_play_success() -> Result<(), fho::Error> {
        let audio_player = ffx_audio_common::tests::fake_audio_player();

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<DeviceResult> = MachineWriter::new_test(None, &test_buffers);

        let selector = Selector::from(fac::Devfs {
            id: "abc123".to_string(),
            device_type: fadevice::DeviceType::Output,
        });

        let (play_remote, play_local) = fidl::Socket::create_datagram();
        let mut async_play_local = fidl::AsyncSocket::from_socket(
            play_local.duplicate_handle(fidl::Rights::SAME_RIGHTS).unwrap(),
        );

        async_play_local.write_all(ffx_audio_common::tests::WAV_HEADER_EXT).await.unwrap();

        device_play(
            audio_player,
            selector,
            play_local,
            play_remote,
            Box::new(&ffx_audio_common::tests::WAV_HEADER_EXT[..]),
            writer,
        )
        .await
        .unwrap();

        let expected_output =
            format!("Successfully processed all audio data. Bytes processed: \"1\"\n");
        let stdout = test_buffers.into_stdout_str();
        assert_eq!(stdout, expected_output);

        Ok(())
    }

    #[fuchsia::test]
    pub async fn test_play_from_file_success() -> Result<(), fho::Error> {
        let audio_player = ffx_audio_common::tests::fake_audio_player();

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

        let (play_remote, play_local) = fidl::Socket::create_datagram();

        let selector = Selector::from(fac::Devfs {
            id: "abc123".to_string(),
            device_type: fadevice::DeviceType::Output,
        });

        device_play(audio_player, selector, play_local, play_remote, Box::new(file_reader), writer)
            .await
            .unwrap();

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
        let test_buffers = TestBuffers::default();
        let mut result_writer: SimpleWriter = SimpleWriter::new_test(&test_buffers);

        let record_command = DeviceRecordCommand {
            duration: Some(std::time::Duration::from_nanos(500)),
            format: Format {
                sample_type: fidl_fuchsia_media::AudioSampleFormat::Unsigned8,
                frames_per_second: 48000,
                channels: 1,
            },
        };

        let selector = Selector::from(fac::Devfs {
            id: "abc123".to_string(),
            device_type: fadevice::DeviceType::Input,
        });

        let (cancel_proxy, cancel_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_audio_controller::RecordCancelerMarker>()
                .unwrap();

        let test_stdout = TestBuffer::default();

        // Pass a future that will never complete as an input waiter.
        let keypress_waiter =
            ffx_audio_common::cancel_on_keypress(cancel_proxy, futures::future::pending().fuse());

        let _res = device_record(
            controller,
            selector,
            record_command,
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
        let test_buffers = TestBuffers::default();
        let mut result_writer: SimpleWriter = SimpleWriter::new_test(&test_buffers);

        let record_command = DeviceRecordCommand {
            duration: None,
            format: Format {
                sample_type: fidl_fuchsia_media::AudioSampleFormat::Unsigned8,
                frames_per_second: 48000,
                channels: 1,
            },
        };

        let selector = Selector::from(fac::Devfs {
            id: "abc123".to_string(),
            device_type: fadevice::DeviceType::Input,
        });

        let (cancel_proxy, cancel_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_audio_controller::RecordCancelerMarker>()
                .unwrap();

        let test_stdout = TestBuffer::default();

        // Test canceler signaling. Not concerned with how much data gets back through socket.
        // Test failing is never finishing execution before timeout.
        let keypress_waiter =
            ffx_audio_common::cancel_on_keypress(cancel_proxy, futures::future::ready(Ok(())));

        let _res = device_record(
            controller,
            selector,
            record_command,
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

        let selector = Selector::from(fac::Devfs {
            id: "abc123".to_string(),
            device_type: fadevice::DeviceType::Input,
        });

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<DeviceResult> =
            MachineWriter::new_test(Some(ffx_writer::Format::Json), &test_buffers);
        let result = device_info(audio_daemon, selector, writer).await;
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

        let selector = Selector::from(fac::Devfs {
            id: "abc123".to_string(),
            device_type: fadevice::DeviceType::Composite,
        });

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<DeviceResult> =
            MachineWriter::new_test(Some(ffx_writer::Format::Json), &test_buffers);
        let result = device_info(audio_daemon, selector, writer).await;
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

    #[fuchsia::test]
    pub async fn test_device_list() -> Result<(), fho::Error> {
        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<DeviceResult> = MachineWriter::new_test(None, &test_buffers);

        let selectors = vec![
            Selector::from(fac::Devfs {
                id: "abc123".to_string(),
                device_type: fadevice::DeviceType::Input,
            }),
            Selector::from(fac::Devfs {
                id: "abc123".to_string(),
                device_type: fadevice::DeviceType::Output,
            }),
        ];

        device_list(selectors, writer).unwrap();

        let stdout = test_buffers.into_stdout_str();
        let stdout_expected = format!(
            "\"/dev/class/audio-input/abc123\" Device id: \"abc123\", Device type: StreamConfig, Input\n\
            \"/dev/class/audio-output/abc123\" Device id: \"abc123\", Device type: StreamConfig, Output\n"
        );

        assert_eq!(stdout, stdout_expected);

        Ok(())
    }

    #[fuchsia::test]
    pub async fn test_device_list_machine() -> Result<(), fho::Error> {
        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<list::ListResult> =
            MachineWriter::new_test(Some(ffx_writer::Format::Json), &test_buffers);

        let selectors = vec![
            Selector::from(fac::Devfs {
                id: "abc123".to_string(),
                device_type: fadevice::DeviceType::Input,
            }),
            Selector::from(fac::Devfs {
                id: "abc123".to_string(),
                device_type: fadevice::DeviceType::Output,
            }),
        ];

        device_list_untagged(selectors, writer).unwrap();

        let stdout = test_buffers.into_stdout_str();
        let stdout_expected = format!(
            "{{\"devices\":[\
                {{\
                    \"device_id\":\"abc123\",\
                    \"is_input\":true,\
                    \"device_type\":\"STREAMCONFIG\",\
                    \"path\":\"/dev/class/audio-input/abc123\"\
                }},\
                {{\
                    \"device_id\":\"abc123\",\
                    \"is_input\":false,\
                    \"device_type\":\"STREAMCONFIG\",\
                    \"path\":\"/dev/class/audio-output/abc123\"\
                }}\
            ]}}\n"
        );

        assert_eq!(stdout, stdout_expected);

        Ok(())
    }
}
