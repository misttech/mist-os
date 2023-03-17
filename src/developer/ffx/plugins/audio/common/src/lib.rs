// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::Error,
    blocking::Unblock,
    errors::ffx_bail,
    fidl::Socket,
    fidl_fuchsia_audio_ffxdaemon::{AudioDaemonPlayRequest, AudioDaemonProxy},
    std::io::BufRead,
};

pub async fn play(
    request: AudioDaemonPlayRequest,
    audio_proxy: AudioDaemonProxy,
    play_local: Socket,
) -> Result<(), Error> {
    let futs = futures::future::try_join(
        async {
            let (stdout_sock, stderr_sock) = match audio_proxy.play(request).await? {
                Ok(value) => (
                    value.stdout.ok_or(anyhow::anyhow!("No stdout socket"))?,
                    value.stderr.ok_or(anyhow::anyhow!("No stderr socket."))?,
                ),
                Err(err) => ffx_bail!("Play failed with err: {}", err),
            };

            let mut stdout = Unblock::new(std::io::stdout());
            let mut stderr = Unblock::new(std::io::stderr());

            futures::future::try_join(
                futures::io::copy(fidl::AsyncSocket::from_socket(stdout_sock)?, &mut stdout),
                futures::io::copy(fidl::AsyncSocket::from_socket(stderr_sock)?, &mut stderr),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Error joining stdio futures: {}", e))
        },
        async move {
            let mut socket_writer = fidl::AsyncSocket::from_socket(play_local)?;
            let stdin_res =
                futures::io::copy(Unblock::new(std::io::stdin()), &mut socket_writer).await;

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
