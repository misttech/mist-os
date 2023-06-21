// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::Error,
    blocking::Unblock,
    errors::ffx_bail,
    fidl::Socket,
    fidl_fuchsia_audio_ffxdaemon::{
        AudioDaemonPlayRequest, AudioDaemonPlayResponse, AudioDaemonProxy, AudioDaemonRequest,
    },
    futures::AsyncReadExt,
    std::borrow::BorrowMut,
    std::io,
    std::io::BufRead,
    std::sync::{Arc, Mutex},
};

lazy_static::lazy_static! {
    pub static ref STDOUT: Arc<Mutex<std::io::Stdout>> = Arc::new(Mutex::new(std::io::stdout()));
    pub static ref STDERR: Arc<Mutex<std::io::Stdout>> = Arc::new(Mutex::new(std::io::stdout()));
}

impl std::io::Write for &'static STDOUT {
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        if let Ok(mut x) = self.borrow_mut().lock() {
            let written = x.write(buf)?;
            Ok(written)
        } else {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "couldn't lock"))
        }
    }

    fn flush(&mut self) -> std::result::Result<(), std::io::Error> {
        // Do nothing since we are not buffered.
        Ok(())
    }
}

impl std::io::Write for &'static STDERR {
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        if let Ok(mut x) = self.borrow_mut().lock() {
            let written = x.write(buf)?;
            Ok(written)
        } else {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "couldn't lock"))
        }
    }

    fn flush(&mut self) -> std::result::Result<(), std::io::Error> {
        // Do nothing since we are not buffered.
        Ok(())
    }
}

pub async fn play<R, W, E>(
    request: AudioDaemonPlayRequest,
    audio_proxy: AudioDaemonProxy,
    play_local: Socket,        // Send data from ffx to target.
    input_reader: R,           // Input generalized to stdin or test buffer. Forward to socket.
    output_writer: &'static W, // Output generalized to stdout or a test buffer. Forward data
    // from daemon to this writer.
    output_error_writer: &'static E, // Likewise, forward error data to a separate writer
                                     // generalized to stderr or a test buffer.
) -> Result<(), Error>
where
    R: std::io::Read + std::marker::Send + 'static,
    W: std::marker::Send + 'static + std::marker::Sync,
    E: std::marker::Send + 'static + std::marker::Sync,
    &'static W: std::io::Write,
    &'static E: std::io::Write,
{
    let futs = futures::future::try_join(
        async {
            let (stdout_sock, stderr_sock) = match audio_proxy.play(request).await? {
                Ok(value) => (
                    value.stdout.ok_or(anyhow::anyhow!("No stdout socket"))?,
                    value.stderr.ok_or(anyhow::anyhow!("No stderr socket."))?,
                ),
                Err(err) => ffx_bail!("Play failed with err: {}", err),
            };

            let mut stdout = Unblock::new(output_writer); // argument must outlive 'static
            let mut stderr = Unblock::new(output_error_writer);

            futures::future::try_join(
                futures::io::copy(fidl::AsyncSocket::from_socket(stdout_sock)?, &mut stdout),
                futures::io::copy(fidl::AsyncSocket::from_socket(stderr_sock)?, &mut stderr),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Error joining stdio futures: {}", e))
        },
        async move {
            let mut socket_writer = fidl::AsyncSocket::from_socket(play_local)?;
            let stdin_res = futures::io::copy(Unblock::new(input_reader), &mut socket_writer).await;

            // Close ffx end of socket so that daemon end reads EOF and stops waiting for data.
            drop(socket_writer);
            stdin_res.map_err(|e| anyhow::anyhow!("Error stdin: {}", e))
        },
    );

    futs.await?;

    Ok(())
}

pub async fn wait_for_keypress(
    canceler: fidl::endpoints::ClientEnd<fidl_fuchsia_audio_ffxdaemon::AudioDaemonCancelerMarker>,
) -> Result<(), std::io::Error> {
    blocking::unblock(move || {
        let mut line = String::new();
        let stdin = std::io::stdin();
        let mut locked = stdin.lock();
        let _ = locked.read_line(&mut line);
    })
    .await;
    let proxy =
        canceler.into_proxy().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    proxy
        .cancel()
        .await
        .and_then(|res| {
            Ok(res.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))))
        })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e)))?
}

pub mod tests {
    use super::*;
    lazy_static::lazy_static! {
        pub static ref MOCK_STDOUT: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::<u8>::new()));
        pub static ref MOCK_STDERR: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::<u8>::new()));
    }

    pub const WAV_HEADER_EXT: &'static [u8; 68] = b"RIFF\xFF\xFF\xFF\xFFWAVE\
    fmt \x28\x00\x00\x00\xfe\xff\x0a\x00\x80\x3e\x00\x00\x00\xe2\x04\x00\
    \x14\x00\x10\x00\x16\x00\x10\x00\xff\x03\x00\x00\x01\x00\x00\x00\
    \x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71\
    data\xFF\xFF\xFF\xFF";

    pub const SINE_WAV: &'static [u8; 140] = b"\
    \x52\x49\x46\x46\x84\x00\x00\x00\x57\x41\x56\x45\x66\x6d\x74\x20\
    \x10\x00\x00\x00\x01\x00\x01\x00\x80\xbb\x00\x00\x80\xbb\x00\x00\
    \x01\x00\x08\x00\x64\x61\x74\x61\x60\x00\x00\x00\x80\x87\x8f\x96\
    \x9d\xa4\xab\xb2\xb9\xbf\xc6\xcc\xd2\xd7\xdc\xe1\xe6\xea\xee\xf2\
    \xf5\xf8\xfa\xfc\xfe\xff\xff\xff\xff\xff\xfe\xfd\xfb\xf9\xf7\xf4\
    \xf0\xec\xe8\xe4\xdf\xda\xd5\xcf\xc9\xc3\xbc\xb6\xaf\xa8\xa1\x9a\
    \x93\x8b\x84\x7d\x75\x6e\x67\x60\x58\x52\x4b\x44\x3e\x38\x32\x2c\
    \x26\x21\x1d\x18\x14\x10\x0d\x0a\x07\x05\x03\x02\x01\x00\x00\x00\
    \x01\x02\x04\x06\x08\x0b\x0e\x11\x15\x1a\x1e\x23";

    impl std::io::Write for &'static MOCK_STDOUT {
        fn write(&mut self, buf: &[u8]) -> std::result::Result<usize, std::io::Error> {
            if let Ok(mut x) = self.borrow_mut().lock() {
                let written = x.write(buf)?;
                Ok(written)
            } else {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "couldn't lock"))
            }
        }

        fn flush(&mut self) -> std::result::Result<(), std::io::Error> {
            // Do nothing since we are not buffered.
            Ok(())
        }
    }

    impl std::io::Write for &'static MOCK_STDERR {
        fn write(&mut self, buf: &[u8]) -> std::result::Result<usize, std::io::Error> {
            if let Ok(mut x) = self.borrow_mut().lock() {
                let written = x.write(buf)?;
                Ok(written)
            } else {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "couldn't lock"))
            }
        }

        fn flush(&mut self) -> std::result::Result<(), std::io::Error> {
            // Do nothing since we are not buffered.
            Ok(())
        }
    }

    pub fn fake_audio_daemon() -> AudioDaemonProxy {
        use futures::AsyncWriteExt;

        let callback = |req| match req {
            AudioDaemonRequest::Play { payload, responder } => {
                let (stdout_remote, stdout_local) = fidl::Socket::create_datagram();
                let (stderr_remote, _stderr_local) = fidl::Socket::create_datagram();
                let response = AudioDaemonPlayResponse {
                    stdout: Some(stdout_remote),
                    stderr: Some(stderr_remote),
                    ..Default::default()
                };

                let data_socket =
                    payload.socket.ok_or(anyhow::anyhow!("Socket argument missing.")).unwrap();

                let mut socket = fidl::AsyncSocket::from_socket(data_socket).unwrap();
                let mut wav_file = vec![0u8; 11];
                fuchsia_async::Task::local(async move {
                    let _ = socket.read_exact(&mut wav_file).await.unwrap();
                })
                .detach();

                responder.send(Ok(response)).unwrap();
                fuchsia_async::Task::local(async move {
                    let mut socket = fidl::AsyncSocket::from_socket(stdout_local).unwrap();
                    let bytes = "Successfully processed all audio data.".as_bytes();
                    socket.write_all(bytes).await.unwrap();
                })
                .detach();
            }
            _ => {}
        };

        fho::testing::fake_proxy(callback)
    }
}
