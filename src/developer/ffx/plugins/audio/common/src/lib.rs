// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    blocking::Unblock,
    fidl::{endpoints::Proxy, Socket},
    fidl_fuchsia_audio_controller::{
        PlayerPlayRequest, PlayerPlayResponse, PlayerProxy, PlayerRequest, RecordCancelerMarker,
    },
    futures::{AsyncReadExt, FutureExt},
    serde::{Deserialize, Serialize},
    std::io::BufRead,
};

#[derive(Debug, Serialize, PartialEq, Deserialize)]
pub struct PlayResult {
    pub bytes_processed: Option<u64>,
}

#[derive(Debug, PartialEq, Serialize)]
pub enum DeviceResult {
    Play(PlayResult),
}

pub async fn play(
    request: PlayerPlayRequest,
    controller: PlayerProxy,
    play_local: Socket, // Send data from ffx to target.
    input_reader: Box<dyn std::io::Read + std::marker::Send + 'static>,
    // Input generalized to stdin or test buffer. Forward to socket.
) -> Result<PlayResult, Error>
where
{
    let (play_result, _stdin_reader_result) = futures::future::try_join(
        async {
            controller
                .play(request)
                .await
                .map_err(|e| anyhow::anyhow!("future try join failed with error {e}"))
        },
        async move {
            let mut socket_writer = fidl::AsyncSocket::from_socket(play_local)?;
            let stdin_res = futures::io::copy(Unblock::new(input_reader), &mut socket_writer).await;

            // Close ffx end of socket so that daemon end reads EOF and stops waiting for data.
            drop(socket_writer);
            stdin_res.map_err(|e| anyhow::anyhow!("Error stdin: {}", e))
        },
    )
    .await?;
    play_result
        .map(|result| PlayResult { bytes_processed: result.bytes_processed })
        .map_err(|e| anyhow::anyhow!("Play failed with error {:?}", e))
}

pub async fn wait_for_keypress(
    canceler: fidl::endpoints::ClientEnd<RecordCancelerMarker>,
) -> Result<(), std::io::Error> {
    let stdin_waiter = blocking::unblock(move || {
        let mut line = String::new();
        let stdin = std::io::stdin();
        let mut locked = stdin.lock();
        let _ = locked.read_line(&mut line);
    })
    .fuse();

    let proxy =
        canceler.into_proxy().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    let closed_fut = async {
        let _ = proxy.on_closed().await;
    }
    .fuse();

    futures::pin_mut!(closed_fut, stdin_waiter);
    futures::select! {
        _res = closed_fut => (),
        _res = stdin_waiter => {
            let _ = proxy.cancel().await.and_then(|res| {
                Ok(res
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))))
            }).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e)))?;

        }
    };

    Ok(())
}

pub mod tests {
    use fidl_fuchsia_audio_controller::DeviceControlProxy;

    use super::*;

    // Header for an infinite-length audio stream: 16 kHz, 3-channel, 16-bit signed.
    // This slice contains a complete WAVE_FORMAT_EXTENSIBLE file header
    // (everything except the audio data itself), which includes the 'fmt ' chunk
    // and the first two fields of the 'data' chunk. After this, the next bytes
    // in this stream would be the audio itself (in 6-byte frames).
    pub const WAV_HEADER_EXT: &'static [u8; 83] = b"\x52\x49\x46\x46\xff\xff\xff\xff
    \x57\x41\x56\x45\x66\x6d\x74\x20\x28\x00\x00\x00\xfe\xff\x03\x00\x80\x3e\x00\x00
    \x00\x77\x01\x00\x06\x00\x10\x00\x16\x00\x10\x00\x07\x00\x00\x00\x01\x00\x00\x00
    \x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71\x64\x61\x74\x61\xff\xff\xff\xff";

    // Complete WAV file (140 bytes in all) for a 48 kHz, 1-channel, unsigned 8-bit
    // audio stream. The file is the old-style PCMWAVEFORMAT and contains 96 frames
    // of a sinusoid of approx. 439 Hz.
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

    pub fn fake_audio_daemon() -> DeviceControlProxy {
        let callback = |req| match req {
            _ => {}
        };
        fho::testing::fake_proxy(callback)
    }

    pub fn fake_audio_player() -> PlayerProxy {
        let callback = |req| match req {
            PlayerRequest::Play { payload, responder } => {
                let data_socket =
                    payload.wav_source.ok_or(anyhow::anyhow!("Socket argument missing.")).unwrap();

                let mut socket = fidl::AsyncSocket::from_socket(data_socket).unwrap();
                let mut wav_file = vec![0u8; 11];
                fuchsia_async::Task::local(async move {
                    let _ = socket.read_exact(&mut wav_file).await.unwrap();
                    // Pass back a fake value for total bytes processed.
                    // ffx tests are focused on the interaction between ffx and the user,
                    // so controller specific functionality can be covered by tests targeting
                    // it specifically.
                    let response =
                        PlayerPlayResponse { bytes_processed: Some(1), ..Default::default() };

                    responder.send(Ok(response)).unwrap();
                })
                .detach();
            }
            _ => {}
        };

        fho::testing::fake_proxy(callback)
    }
}
