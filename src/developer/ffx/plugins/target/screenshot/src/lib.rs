// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, bail, Context, Result},
    chrono::{Datelike, Local, Timelike},
    ffx_core::ffx_plugin,
    ffx_target_screenshot_args::{Format, ScreenshotCommand},
    fidl_fuchsia_io as fio,
    fidl_fuchsia_math::SizeU,
    fidl_fuchsia_ui_composition::{ScreenshotFormat, ScreenshotProxy, ScreenshotTakeFileRequest},
    futures::stream::{FuturesOrdered, StreamExt},
    png::HasParameters,
    std::convert::{TryFrom, TryInto},
    std::fs,
    std::io::BufWriter,
    std::io::Write,
    std::path::{Path, PathBuf},
};

// Reads all of the contents of the given file from the current seek
// offset to end of file, returning the content. It errors if the seek pointer
// starts at an offset that results in reading less number of bytes read
// from the initial seek offset by the first request made by this function to EOF.
pub async fn read_data(file: &fio::FileProxy) -> Result<Vec<u8>> {
    // Number of concurrent read operations to maintain (aim for a 128kb
    // in-flight buffer, divided by the fuchsia.io chunk size). On a short range
    // network, 64kb should be more than sufficient, but on an LFN such as a
    // work-from-home scenario, having some more space further optimizes
    // performance.
    const CONCURRENCY: u64 = 131072 / fio::MAX_BUF;

    let mut out = Vec::new();

    let (status, attrs) = file
        .get_attr()
        .await
        .context(format!("Error: Failed to get attributes of file (fidl failure)"))?;

    if status != 0 {
        bail!("Error: Failed to get attributes, status: {}", status);
    }

    let mut queue = FuturesOrdered::new();

    for _ in 0..CONCURRENCY {
        queue.push(file.read(fio::MAX_BUF));
    }

    loop {
        let mut bytes: Vec<u8> = queue
            .next()
            .await
            .context("read stream closed prematurely")??
            .map_err(|status: i32| anyhow!("read error: status={}", status))?;

        if bytes.is_empty() {
            break;
        }
        out.append(&mut bytes);

        while queue.len() < CONCURRENCY.try_into().unwrap() {
            queue.push(file.read(fio::MAX_BUF));
        }
    }

    if out.len() != usize::try_from(attrs.content_size).map_err(|e| anyhow!(e))? {
        bail!("Error: Expected {} bytes, but instead read {} bytes", attrs.content_size, out.len());
    }

    Ok(out)
}

#[ffx_plugin(ScreenshotProxy = "core/ui:expose:fuchsia.ui.composition.Screenshot")]
pub async fn screenshot(screenshot_proxy: ScreenshotProxy, cmd: ScreenshotCommand) -> Result<()> {
    screenshot_impl(screenshot_proxy, cmd, &mut std::io::stdout()).await
}

pub async fn screenshot_impl<W: Write>(
    screenshot_proxy: ScreenshotProxy,
    cmd: ScreenshotCommand,
    writer: &mut W,
) -> Result<()> {
    let mut screenshot_file_path = match cmd.output_directory {
        Some(file_dir) => {
            let dir = Path::new(&file_dir);
            if !dir.is_dir() {
                bail!("ERROR: Path provided is not a directory");
            }
            dir.to_path_buf().join("screenshot")
        }
        None => {
            let dir = default_output_dir();
            fs::create_dir_all(&dir)?;
            dir.to_path_buf().join("screenshot")
        }
    };

    // TODO(fxbug.dev/108647): Use rgba format when available.
    // TODO(fxbug.dev/103742): Use png format when available.
    let screenshot_response = screenshot_proxy
        .take_file(ScreenshotTakeFileRequest {
            format: Some(ScreenshotFormat::BgraRaw),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("Error: Could not get the screenshot from the target: {:?}", e))?;

    let img_size = screenshot_response.size.expect("no data size returned from screenshot");
    let client_end = screenshot_response.file.expect("no file returned from screenshot");

    let file_proxy = client_end.into_proxy().expect("could not create file proxy");

    let mut img_data = read_data(&file_proxy).await?;

    match cmd.format {
        Format::PNG => {
            save_as_png(&mut screenshot_file_path, &mut img_data, img_size);
        }
        Format::BGRA => {
            save_as_bgra(&mut screenshot_file_path, &mut img_data);
        }
        Format::RGBA => {
            save_as_rgba(&mut screenshot_file_path, &mut img_data);
        }
    };

    writeln!(writer, "Exported {}", screenshot_file_path.to_string_lossy())?;

    Ok(())
}

fn default_output_dir() -> PathBuf {
    let now = Local::now();

    Path::new("/tmp").join("screenshot").join(format!(
        "{}{:02}{:02}_{:02}{:02}{:02}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    ))
}

fn create_file(screenshot_file_path: &mut PathBuf) -> fs::File {
    fs::File::create(screenshot_file_path.clone())
        .unwrap_or_else(|_| panic!("cannot create file {}", screenshot_file_path.to_string_lossy()))
}

fn save_as_bgra(screenshot_file_path: &mut PathBuf, img_data: &mut Vec<u8>) {
    screenshot_file_path.set_extension("bgra");
    let mut screenshot_file = create_file(screenshot_file_path);

    screenshot_file.write_all(&img_data[..]).expect("failed to write bgra image data.");
}

// TODO(fxbug.dev/108647): Use rgba format when available.
fn save_as_rgba(screenshot_file_path: &mut PathBuf, img_data: &mut Vec<u8>) {
    bgra_to_rbga(img_data);

    screenshot_file_path.set_extension("rgba");
    let mut screenshot_file = create_file(screenshot_file_path);
    screenshot_file.write_all(&img_data[..]).expect("failed to write rgba image data.");
}

// TODO(fxbug.dev/103742): Use png format when available.
fn save_as_png(screenshot_file_path: &mut PathBuf, img_data: &mut Vec<u8>, img_size: SizeU) {
    bgra_to_rbga(img_data);

    screenshot_file_path.set_extension("png");
    let screenshot_file = create_file(screenshot_file_path);

    let ref mut w = BufWriter::new(screenshot_file);
    let mut encoder = png::Encoder::new(w, img_size.width, img_size.height);

    encoder.set(png::BitDepth::Eight);
    encoder.set(png::ColorType::RGBA);
    let mut png_writer = encoder.write_header().unwrap();
    png_writer.write_image_data(&img_data).expect("failed to write image data as PNG");
}

/// Performs inplace BGRA -> RGBA.
fn bgra_to_rbga(img_data: &mut Vec<u8>) {
    let bytes_per_pixel = 4;
    let mut blue_pos = 0;
    let mut red_pos = 2;
    let img_data_size = img_data.len();

    while blue_pos < img_data_size && red_pos < img_data_size {
        let blue = img_data[blue_pos];
        img_data[blue_pos] = img_data[red_pos];
        img_data[red_pos] = blue;
        blue_pos = blue_pos + bytes_per_pixel;
        red_pos = red_pos + bytes_per_pixel;
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use {
        super::*,
        fidl::endpoints::ServerEnd,
        fidl_fuchsia_ui_composition::{ScreenshotRequest, ScreenshotTakeFileResponse},
        futures::TryStreamExt,
        tempfile::tempdir,
    };

    fn serve_fake_file(server: ServerEnd<fio::FileMarker>) {
        fuchsia_async::Task::local(async move {
            let data: [u8; 16] = [1, 2, 3, 4, 1, 2, 3, 4, 4, 3, 2, 1, 4, 3, 2, 1];
            let mut stream =
                server.into_stream().expect("converting fake file server proxy to stream");

            let mut cc: u32 = 0;
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    fio::FileRequest::Read { count: _, responder } => {
                        cc = cc + 1;
                        if cc == 1 {
                            responder
                                .send(&mut Ok(data.to_vec()))
                                .expect("writing file test response");
                        } else {
                            responder.send(&mut Ok(vec![])).expect("writing file test response");
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

    fn setup_fake_screenshot_server() -> ScreenshotProxy {
        setup_fake_screenshot_proxy(move |req| match req {
            ScreenshotRequest::TakeFile { payload: _, responder } => {
                let mut screenshot = ScreenshotTakeFileResponse::default();

                let (file_client_end, file_server_end) =
                    fidl::endpoints::create_endpoints::<fio::FileMarker>();

                let _ = screenshot.file.insert(file_client_end);
                let _ = screenshot.size.insert(SizeU { width: 2, height: 2 });
                serve_fake_file(file_server_end);
                responder.send(screenshot).unwrap();
            }

            _ => assert!(false),
        })
    }

    async fn run_screenshot_test(cmd: ScreenshotCommand) {
        let screenshot_proxy = setup_fake_screenshot_server();

        let mut writer = Vec::new();
        let file_format = cmd.format.clone();
        let output_dir = cmd.output_directory.clone();
        let result = screenshot_impl(screenshot_proxy, cmd, &mut writer).await;
        assert!(result.is_ok());

        let output = String::from_utf8(writer).unwrap();

        let output_vec = output.split_whitespace().collect::<Vec<_>>();
        assert!(output_vec.len() == 2);

        assert!(output_vec[0].eq_ignore_ascii_case("Exported"));

        // output_vec[1] holds the path to the exported file
        match output_dir {
            Some(dir) => {
                assert!(output_vec[1].starts_with(dir.as_str()))
            }
            None => (),
        };

        match file_format {
            Format::PNG => {
                assert!(output_vec[1].ends_with("screenshot.png"));
            }
            Format::BGRA => {
                assert!(output_vec[1].ends_with("screenshot.bgra"));
            }
            Format::RGBA => {
                assert!(output_vec[1].ends_with("screenshot.rgba"));
            }
        }

        let file_path = Path::new(output_vec[1]);

        assert!(file_path.is_file());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_bgra() -> Result<()> {
        run_screenshot_test(ScreenshotCommand { output_directory: None, format: Format::BGRA })
            .await;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_output_dir_bgra() -> Result<()> {
        // Create a test directory in TempFile::tempdir.
        let output_dir = PathBuf::from(tempdir().unwrap().path()).join("screenshot_test");
        fs::create_dir_all(&output_dir)?;
        run_screenshot_test(ScreenshotCommand {
            output_directory: Some(output_dir.to_string_lossy().to_string()),
            format: Format::BGRA,
        })
        .await;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_png() -> Result<()> {
        run_screenshot_test(ScreenshotCommand { output_directory: None, format: Format::PNG })
            .await;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_output_dir_png() -> Result<()> {
        // Create a test directory in TempFile::tempdir.
        let output_dir = PathBuf::from(tempdir().unwrap().path()).join("screenshot_test");
        fs::create_dir_all(&output_dir)?;
        run_screenshot_test(ScreenshotCommand {
            output_directory: Some(output_dir.to_string_lossy().to_string()),
            format: Format::PNG,
        })
        .await;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_rgba() -> Result<()> {
        run_screenshot_test(ScreenshotCommand { output_directory: None, format: Format::RGBA })
            .await;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_output_dir_rgba() -> Result<()> {
        // Create a test directory in TempFile::tempdir.
        let output_dir = PathBuf::from(tempdir().unwrap().path()).join("screenshot_test");
        fs::create_dir_all(&output_dir)?;
        run_screenshot_test(ScreenshotCommand {
            output_directory: Some(output_dir.to_string_lossy().to_string()),
            format: Format::RGBA,
        })
        .await;
        Ok(())
    }
}
