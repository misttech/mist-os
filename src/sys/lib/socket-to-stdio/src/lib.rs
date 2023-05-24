// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Context as _,
    fidl::HandleBased as _,
    fidl_fuchsia_io as fio,
    futures::{AsyncReadExt as _, AsyncWriteExt as _, FutureExt as _},
    std::io::StdoutLock,
    termion::raw::IntoRawMode as _,
};

/// Abstracts stdout for `connect_socket_to_stdio`. Allows callers to determine if stdout should be
/// exclusively owned for the duration of the call.
pub enum Stdout<'a> {
    /// Exclusive ownership of stdout (nothing else can write to stdout while this exists),
    /// put into raw mode.
    Raw(termion::raw::RawTerminal<StdoutLock<'a>>),
    /// Shared ownership of stdout (output may be interleaved with output from other sources).
    Buffered,
}

impl std::io::Write for Stdout<'_> {
    fn flush(&mut self) -> Result<(), std::io::Error> {
        match self {
            Self::Raw(r) => r.flush(),
            Self::Buffered => std::io::stdout().flush(),
        }
    }
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        match self {
            Self::Raw(r) => r.write(buf),
            Self::Buffered => std::io::stdout().write(buf),
        }
    }
}

impl Stdout<'_> {
    pub fn raw() -> anyhow::Result<Self> {
        let stdout = std::io::stdout();

        if !termion::is_tty(&stdout) {
            anyhow::bail!("interactive mode does not support piping");
        }

        // Put the host terminal into raw mode, so input characters are not echoed, streams are not
        // buffered and newlines are not changed.
        let term_out =
            stdout.lock().into_raw_mode().context("could not set raw mode on terminal")?;

        Ok(Self::Raw(term_out))
    }

    pub fn buffered() -> Self {
        Self::Buffered
    }
}

/// Concurrently:
///   * locks stdin and copies the input to `socket`
///   * reads data from `socket` and writes it to `stdout`
/// Finishes when either of those tasks finish.
/// `socket` must have ZX_RIGHT_DUPLICATE.
pub async fn connect_socket_to_stdio(
    socket: fidl::Socket,
    stdout: Stdout<'_>,
) -> anyhow::Result<()> {
    connect_socket_to_stdio_impl(socket, || std::io::stdin().lock(), stdout)?.await
}

fn connect_socket_to_stdio_impl<R>(
    socket: fidl::Socket,
    stdin: impl FnOnce() -> R + Send + 'static,
    mut stdout: impl std::io::Write,
) -> anyhow::Result<impl futures::Future<Output = anyhow::Result<()>>>
where
    R: std::io::Read,
{
    // Use a separate thread to read from stdin without blocking the executor.
    //
    // Instead of duplicating the socket with a syscall, we could create a fuchsia_async::Socket
    // and then use AsyncReadExt::split to create read and write halves. The downside to this
    // approach is that fuchsia_async::Socket::from_socket registers the socket as a receiver of
    // the current thread's executor and receivers must not outlive their executor. So, we would
    // then need to make sure the socket passed to the thread created by fuchsia_async::unblock
    // is dropped before the calling thread's executor is dropped. We could do this by having
    // the future returned by this fn not complete until the socket is dropped, but that fails if
    // the caller does not poll the returned future to completion (and the failure presents as a
    // panic [0] when the calling thread's executor is dropped and it would be difficult to trace
    // the error back to this).
    //
    // The downside to the duplicate_handle approach is that it requires the socket to have
    // ZX_RIGHT_DUPLICATE.
    //
    // [0] https://cs.opensource.google/fuchsia/fuchsia/+/main:src/lib/fuchsia-async/src/runtime/fuchsia/executor/common.rs;l=282-285;drc=ab0b08e52812d159026dc7e101620d57d7aead10
    #[cfg(target_os = "fuchsia")]
    let rights = fidl::handle::fuchsia_handles::Rights::SAME_RIGHTS;
    #[cfg(not(target_os = "fuchsia"))]
    let rights = fidl::handle::non_fuchsia_handles::Rights::SAME_RIGHTS;

    let socket_out = socket.duplicate_handle(rights).context("duplicating socket")?;
    let mut socket_in =
        fuchsia_async::Socket::from_socket(socket).context("creating async socket_in")?;

    let stdin_to_socket = fuchsia_async::unblock(move || {
        let mut executor = fuchsia_async::LocalExecutor::new();
        executor.run_singlethreaded(async move {
            let mut socket_out = fuchsia_async::Socket::from_socket(socket_out)
                .context("creating async socket_out")?;
            let mut stdin = stdin();
            let mut buf = [0u8; fio::MAX_BUF as usize];
            loop {
                let bytes_read = stdin.read(&mut buf).context("reading from stdin")?;
                if bytes_read == 0 {
                    return Ok::<(), anyhow::Error>(());
                }
                socket_out.write_all(&buf[..bytes_read]).await.context("writing to socket")?;
                socket_out.flush().await.context("flushing socket")?;
            }
        })?;
        Ok::<(), anyhow::Error>(())
    });

    Ok(async move {
        futures::select! {
            res = stdin_to_socket.fuse() => res,
            res = async move {
                loop {
                    let mut buf = [0u8; fio::MAX_BUF as usize];
                    let bytes_read = socket_in.read(&mut buf).await.context("reading from socket")?;
                    if bytes_read == 0 {
                        break;
                    }
                    stdout.write_all(&buf[..bytes_read]).context("writing to stdout")?;
                    stdout.flush().context("flushing stdout")?;
                }
                Ok(())
            }.fuse() => res,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    async fn stdin_to_socket() {
        let (socket, socket_remote) = fidl::Socket::create_stream();

        let () = connect_socket_to_stdio_impl(socket_remote, || &b"test input"[..], vec![])
            .unwrap()
            .await
            .unwrap();

        let mut out = vec![0u8; 100];
        let bytes_read;
        loop {
            match socket.read(&mut out) {
                Ok(n) => {
                    bytes_read = n;
                    break;
                }
                Err(fidl::Status::SHOULD_WAIT) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }

        assert_eq!(bytes_read, 10);
        assert_eq!(&out[..bytes_read], &b"test input"[..]);
    }

    #[fuchsia::test]
    async fn socket_to_stdout() {
        let (socket, socket_remote) = fidl::Socket::create_stream();
        assert_eq!(socket.write(&b"test input"[..]).unwrap(), 10);
        drop(socket);
        let mut stdout = vec![];
        let (unblocker, block_until) = std::sync::mpsc::channel();

        let () = connect_socket_to_stdio_impl(
            socket_remote,
            move || {
                let () = block_until.recv().unwrap();
                &[][..]
            },
            &mut stdout,
        )
        .unwrap()
        .await
        .unwrap();

        // let the stdin_to_socket thread finish before test cleanup
        unblocker.send(()).unwrap();

        assert_eq!(&stdout[..], &b"test input"[..]);
    }
}
