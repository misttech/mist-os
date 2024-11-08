// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use chrono::{Datelike, Local, Timelike};
use ffx_snapshot_args::SnapshotCommand;
use fho::{
    bug, moniker, return_bug, return_user_error, Error, FfxMain, FfxTool, Result,
    VerifiedMachineWriter,
};
use fidl_fuchsia_feedback::{
    Annotation, DataProviderProxy, GetAnnotationsParameters, GetSnapshotParameters,
};
use fidl_fuchsia_io as fio;
use futures::stream::{FuturesOrdered, StreamExt};
use schemars::JsonSchema;
use serde::Serialize;
use std::env::temp_dir;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Snapshot captured in specified file.
    Snapshot { output_file: PathBuf },
    /// Annotations
    Annotations { annotations: String },
    /// Unexpected error with string.
    UnexpectedError { message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { message: String },
}

#[derive(FfxTool)]
pub struct SnapshotTool {
    #[command]
    cmd: SnapshotCommand,
    #[with(moniker("/core/feedback"))]
    data_provider_proxy: DataProviderProxy,
}

fho::embedded_plugin!(SnapshotTool);

#[async_trait(?Send)]
impl FfxMain for SnapshotTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        if self.cmd.dump_annotations {
            match dump_annotations(self.data_provider_proxy).await {
                Ok(s) => {
                    writer.machine_or(&CommandStatus::Annotations { annotations: s.clone() }, s)
                }
                Err(Error::User(e)) => writer.machine_or_else(
                    &CommandStatus::UserError { message: e.to_string() },
                    || {
                        return Error::User(e);
                    },
                ),
                Err(e) => writer.machine_or_else(
                    &CommandStatus::UnexpectedError { message: e.to_string() },
                    || return e,
                ),
            }?;
        } else {
            match snapshot_impl(self.data_provider_proxy, self.cmd).await {
                Ok(filepath) => writer.machine_or(
                    &CommandStatus::Snapshot { output_file: filepath.clone() },
                    format!("Exported {}", filepath.to_string_lossy()),
                ),
                Err(Error::User(e)) => writer.machine_or_else(
                    &CommandStatus::UserError { message: e.to_string() },
                    || {
                        return Error::User(e);
                    },
                ),
                Err(e) => writer.machine_or_else(
                    &CommandStatus::UnexpectedError { message: e.to_string() },
                    || return e,
                ),
            }?;
        }
        Ok(())
    }
}

// read_data reads all of the contents of the given file from the current seek
// offset to end of file, returning the content. It errors if the seek pointer
// starts at an offset that results in reading less than the size of the file as
// reported on by the first request made by this function.
//
// The implementation attempts to maintain 8 concurrent in-flight requests so as
// to overcome the BDP that otherwise leads to a performance problem with a
// networked peer and only 8kb buffers in fuchsia.io.
pub async fn read_data(file: &fio::FileProxy) -> Result<Vec<u8>> {
    // Number of concurrent read operations to maintain (aim for a 128kb
    // in-flight buffer, divided by the fuchsia.io chunk size). On a short range
    // network, 64kb should be more than sufficient, but on an LFN such as a
    // work-from-home scenario, having some more space further optimizes
    // performance.
    const CONCURRENCY: u64 = 131072 / fio::MAX_BUF;

    let mut out = Vec::new();

    let (status, attrs) =
        file.get_attr().await.map_err(|e| bug!("Failed to get attributes of file: {e}"))?;

    if status != 0 {
        return_bug!("Error: Failed to get attributes, status: {}", status);
    }

    let mut queue = FuturesOrdered::new();

    for _ in 0..CONCURRENCY {
        queue.push_back(file.read(fio::MAX_BUF));
    }

    loop {
        if let Some(resp) = queue.next().await {
            let mut bytes: Vec<u8> = resp
                .map_err(|e| bug!("read stream error {e}"))?
                .map_err(|status: i32| bug!("read error: status={status}"))?;

            if bytes.is_empty() {
                break;
            }
            out.append(&mut bytes);
        }

        while queue.len() < CONCURRENCY.try_into().unwrap() {
            queue.push_back(file.read(fio::MAX_BUF));
        }
    }

    if out.len() != usize::try_from(attrs.content_size).map_err(|e| bug!("{e}"))? {
        return_bug!(
            "Error: Expected {} bytes, but instead read {} bytes",
            attrs.content_size,
            out.len()
        );
    }

    Ok(out)
}

// Build a multi-line string that represets the current annotation.
fn format_annotation(previous_key: &String, new_key: &String, new_value: &String) -> String {
    let mut output = String::from("");
    let old_key_vec: Vec<_> = previous_key.split(".").collect();
    let new_key_vec: Vec<_> = new_key.split(".").collect();

    let mut common_root = true;
    for idx in 0..new_key_vec.len() {
        // ignore shared key segments.
        if common_root && idx < old_key_vec.len() {
            if old_key_vec[idx] == new_key_vec[idx] {
                continue;
            }
        }
        common_root = false;

        // Build the formatted line from the key segment and append it to the output.
        let indentation: String = (0..idx).map(|_| "    ").collect();
        let end_of_key = new_key_vec.len() - 1 == idx;
        let line = match end_of_key {
            false => format!("{}{}\n", indentation, &new_key_vec[idx]),
            true => format!("{}{}: {}\n", indentation, &new_key_vec[idx], new_value),
        };
        output.push_str(&line);
    }

    output
}

fn format_annotations(mut annotations: Vec<Annotation>) -> String {
    let mut output = String::from("");

    // make sure annotations are sorted.
    annotations.sort_by(|a, b| a.key.cmp(&b.key));

    let mut previous_key = String::from("");
    for annotation in annotations {
        let segment = format_annotation(&previous_key, &annotation.key, &annotation.value);
        output.push_str(&segment);
        previous_key = annotation.key;
    }

    output
}

pub async fn dump_annotations(data_provider_proxy: DataProviderProxy) -> Result<String> {
    // Build parameters
    let params = GetAnnotationsParameters {
        collection_timeout_per_annotation: Some(
            i64::try_from(Duration::from_secs(60).as_nanos()).map_err(|e| bug!(e))?,
        ),
        ..Default::default()
    };

    // Request annotations.
    let annotations = data_provider_proxy
        .get_annotations(&params)
        .await
        .map_err(|e| bug!("Could not get the annotations from the target: {e:?}"))?
        .annotations2
        .ok_or(bug!("Received empty annotations."))?;

    Ok(format_annotations(annotations))
}

pub async fn snapshot_impl(
    data_provider_proxy: DataProviderProxy,
    cmd: SnapshotCommand,
) -> Result<PathBuf> {
    let output_dir = match cmd.output_file {
        None => {
            let dir = default_output_dir();
            fs::create_dir_all(&dir).map_err(|e| bug!(e))?;
            dir
        }
        Some(file_dir) => {
            let dir = Path::new(&file_dir);
            if !dir.is_dir() {
                return_user_error!("Path provided is not a directory: {file_dir}");
            }
            dir.to_path_buf()
        }
    };

    // Make file proxy and channel for snapshot
    let (file_proxy, file_server_end) =
        fidl::endpoints::create_proxy::<fio::FileMarker>().map_err(|e| bug!(e))?;

    // Build parameters
    let params = GetSnapshotParameters {
        collection_timeout_per_data: Some(
            i64::try_from(Duration::from_secs(60).as_nanos()).map_err(|e| bug!(e))?,
        ),
        response_channel: Some(file_server_end.into_channel()),
        ..Default::default()
    };

    // Request snapshot & send channel.
    let _snapshot = data_provider_proxy
        .get_snapshot(params)
        .await
        .map_err(|e| bug!("Error: Could not get the snapshot from the target: {e:?}"))?;

    // Read archive
    let data = read_data(&file_proxy).await?;

    // Write archive to file.
    let file_path = output_dir.join("snapshot.zip");
    let mut file = fs::File::create(&file_path).map_err(|e| bug!(e))?;
    file.write_all(&data).map_err(|e| bug!(e))?;

    Ok(file_path)
}

fn default_output_dir() -> PathBuf {
    let now = Local::now();
    let tmpdir = temp_dir();
    tmpdir.join("snapshots").join(format!(
        "{}{:02}{:02}_{:02}{:02}{:02}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    ))
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use fho::{Format, TestBuffers};
    use fidl::endpoints::ServerEnd;
    use fidl_fuchsia_feedback::{Annotations, DataProviderRequest, Snapshot};
    use futures::TryStreamExt;

    fn serve_fake_file(server: ServerEnd<fio::FileMarker>) {
        fuchsia_async::Task::local(async move {
            let data: [u8; 3] = [1, 2, 3];
            let mut stream =
                server.into_stream().expect("converting fake file server proxy to stream");

            let mut cc: u32 = 0;
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    fio::FileRequest::Read { count: _, responder } => {
                        cc = cc + 1;
                        if cc == 1 {
                            responder.send(Ok(&data)).expect("writing file test response");
                        } else {
                            responder.send(Ok(&[])).expect("writing file test response");
                        }
                    }
                    fio::FileRequest::GetAttr { responder } => {
                        let attrs = fio::NodeAttributes {
                            mode: 0,
                            id: 0,
                            content_size: data.len() as u64,
                            storage_size: data.len() as u64,
                            link_count: 1,
                            creation_time: 0,
                            modification_time: 0,
                        };
                        responder.send(0, &attrs).expect("sending attributes");
                    }
                    e => panic!("not supported {:?}", e),
                }
            }
        })
        .detach();
    }

    macro_rules! annotation {
        ($val_1:expr, $val_2:expr) => {
            Annotation { key: $val_1.to_string(), value: $val_2.to_string() }
        };
    }

    fn setup_fake_data_provider_server(annotations: Annotations) -> DataProviderProxy {
        fho::testing::fake_proxy(move |req| match req {
            DataProviderRequest::GetSnapshot { params, responder } => {
                let channel = params.response_channel.unwrap();
                let server_end = ServerEnd::<fio::FileMarker>::new(channel);

                serve_fake_file(server_end);

                let snapshot = Snapshot::default();
                responder.send(snapshot).unwrap();
            }
            DataProviderRequest::GetAnnotations { params, responder } => {
                let _ignore = params;
                responder.send(&annotations).unwrap();
            }
            _ => assert!(false),
        })
    }

    #[fuchsia::test]
    async fn test_snaphot() {
        let annotations = Annotations::default();
        let data_provider_proxy = setup_fake_data_provider_server(annotations);

        let cmd = SnapshotCommand { output_file: None, dump_annotations: false };
        let result = snapshot_impl(data_provider_proxy, cmd).await;
        assert!(result.is_ok());
        let output = result.expect("snapshot path");
        assert!(output.ends_with("snapshot.zip"));
    }

    #[fuchsia::test]
    async fn test_snaphot_machine() {
        let annotations = Annotations::default();
        let data_provider_proxy = setup_fake_data_provider_server(annotations);
        let tempdir = default_output_dir();
        fs::create_dir_all(&tempdir).expect("temp dir");
        let tool = SnapshotTool {
            cmd: SnapshotCommand {
                output_file: Some(tempdir.to_string_lossy().to_string()),
                dump_annotations: false,
            },
            data_provider_proxy,
        };
        let buffers = TestBuffers::default();
        let writer = VerifiedMachineWriter::<CommandStatus>::new_test(Some(Format::Json), &buffers);

        let result = tool.main(writer).await;
        assert!(result.is_ok());
        let output = buffers.into_stdout_str();
        assert_eq!(
            output,
            format!(
                "{{\"snapshot\":{{\"output_file\":\"{}/snapshot.zip\"}}}}\n",
                tempdir.to_string_lossy()
            )
        );
    }

    #[fuchsia::test]
    async fn test_annotations() -> Result<()> {
        let annotation_vec: Vec<Annotation> = vec![
            annotation!("build.board", "x64"),
            annotation!("hardware.board.name", "default-board"),
            annotation!("build.is_debug", "false"),
        ];
        let annotations = Annotations { annotations2: Some(annotation_vec), ..Default::default() };
        let data_provider_proxy = setup_fake_data_provider_server(annotations);

        let output = dump_annotations(data_provider_proxy).await?;
        assert_eq!(
            output,
            "build\n\
        \x20   board: x64\n\
        \x20   is_debug: false\n\
        hardware\n\
        \x20   board\n\
        \x20       name: default-board\n"
        );
        Ok(())
    }
}
