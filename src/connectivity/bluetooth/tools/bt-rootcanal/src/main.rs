// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context, Error},
    argh::FromArgs,
    fidl_fuchsia_hardware_bluetooth::VirtualControllerMarker,
    fuchsia_async::{self as fasync, net::TcpStream, pin_mut},
    fuchsia_fs::OpenFlags,
    fuchsia_zircon::{self as zx},
    futures::{
        self, future::Either, io::ReadHalf, io::WriteHalf, AsyncRead, AsyncReadExt, AsyncWrite,
        AsyncWriteExt, FutureExt,
    },
    std::net::{IpAddr, SocketAddr},
    std::str::FromStr,
};

// Across all three link types, ACL has the largest frame at 1028. Add a byte of UART header.
const UART_MAX_FRAME_BUFFER_SIZE: usize = 1029;

// Default rootcanal TCP port.
const fn default_port() -> u16 {
    6402
}

// Default control device.
fn default_control_device() -> String {
    // TODO(303503457): Access virtual device via "/dev/class/bt-hci-virtual".
    "sys/platform/00:00:30/bt_hci_virtual".to_string()
}

/// Define the command line arguments that the tool accepts.
#[derive(FromArgs)]
#[argh(description = "Bluetooth Root Canal Proxy")]
struct Options {
    /// Control Device
    #[argh(
        option,
        short = 'c',
        description = "control device path",
        default = "default_control_device()"
    )]
    control_device: String,
    /// Host Port (default 6402)
    #[argh(
        option,
        short = 'p',
        description = "host port number (default 6402)",
        default = "default_port()"
    )]
    port: u16,
    /// Host IP address
    #[argh(positional, description = "host IP address")]
    host: String,
}

/// Reads the TCP stream from the host from the `read_stream` and writes all data to the loopback
/// driver over `channel`.
async fn stream_reader(
    mut read_stream: ReadHalf<impl AsyncRead>,
    channel: &fasync::Channel,
) -> Result<(), Error> {
    let mut buf = [0u8; UART_MAX_FRAME_BUFFER_SIZE];
    let mut handles = Vec::new();
    loop {
        let size = read_stream
            .read(&mut buf)
            .await
            .map_err(|e| anyhow!("Unable to read TCP channel {:?}", e))?;
        if size == 0 {
            return Err(anyhow!("Read zero bytes. Stream may be closed"));
        }
        channel
            .write(&buf[0..size], &mut handles)
            .map_err(|e| anyhow!("Unable to write to emulator channel {:?}", e))?;
    }
}

/// Reads the `channel` from the loopback device and writes all data to TCP stream to the host
/// over the `write_stream`.
async fn channel_reader(
    mut write_stream: WriteHalf<impl AsyncWrite>,
    channel: &fasync::Channel,
) -> Result<(), Error> {
    loop {
        let mut buffer = zx::MessageBuf::new();
        channel
            .recv_msg(&mut buffer)
            .await
            .map_err(|e| anyhow!("Error read from channel {:?}", e))?;
        let _size = write_stream
            .write(buffer.bytes())
            .await
            .map_err(|e| anyhow!("Unable to write to TCP stream {:?}", e))?;
    }
}

// Runs reader futures on both ends.
async fn run_rootcanal(
    stream: impl AsyncRead + AsyncWrite + Sized,
    channel: fasync::Channel,
) -> Result<(), Error> {
    let (read_stream, write_stream) = stream.split();

    let chan_fut = channel_reader(write_stream, &channel).fuse();
    pin_mut!(chan_fut);

    let stream_fut = stream_reader(read_stream, &channel).fuse();
    pin_mut!(stream_fut);

    match futures::future::select(chan_fut, stream_fut).await {
        Either::Left((res, _)) => res,
        Either::Right((res, _)) => res,
    }
}

/// Opens the virtual loopback device, creates a channel to pass to it and returns that channel.
async fn open_virtual_device(control_device: &str) -> Result<fasync::Channel, Error> {
    let dev_directory = fuchsia_fs::directory::open_in_namespace("/dev", OpenFlags::RIGHT_READABLE)
        .expect("unable to open directory");

    let controller = device_watcher::recursive_wait_and_open::<VirtualControllerMarker>(
        &dev_directory,
        control_device,
    )
    .await
    .with_context(|| format!("failed to open {}", control_device))?;

    let (remote_channel, local_channel) = zx::Channel::create();
    controller.create_loopback_device(remote_channel)?;
    fasync::Channel::from_channel(local_channel)
        .map_err(|e| anyhow!("Error opening virtual device {:?}", e))
}

/// Connects to the socket addr and loopack device and runs the loop proxing data between both.
#[fuchsia::main(logging_tags = ["rootcanal"])]
async fn main() -> Result<(), Error> {
    let opt: Options = argh::from_env();

    let channel = open_virtual_device(&opt.control_device).await?;
    let socket_addr: SocketAddr = (IpAddr::from_str(&opt.host)?, opt.port).into();

    eprintln!("Opening host {}", socket_addr);
    let stream = TcpStream::connect(socket_addr)?.await.expect("unable to connect to host");
    eprintln!("Connected");

    run_rootcanal(stream, channel).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Poll;

    #[fuchsia::test]
    fn test_bidirectional_comms() {
        let mut exec = fasync::TestExecutor::new();

        // Mock channel setup
        let (txc, rxc) = zx::Channel::create();
        let async_channel = fasync::Channel::from_channel(rxc).expect("unable to unwrap");

        let (txs, rxs) = zx::Socket::create_stream();
        let async_socket = fasync::Socket::from_socket(rxs);

        let fut = Box::pin(super::run_rootcanal(async_socket, async_channel).fuse());
        pin_mut!(fut);

        // Run with nothing to read yet. Futures should be waiting on both streams.
        assert!(exec.run_until_stalled(&mut fut).is_pending());

        // Write to the channel
        let mut handles = Vec::new();
        let bytes = [0x01, 0x02, 0x03, 0x04];
        txc.write(&bytes, &mut handles).expect("write failed");

        // Pump to read bytes from channel and write to the socket.
        assert!(exec.run_until_stalled(&mut fut).is_pending());

        // Read from the socket
        let mut read_buf: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
        assert_eq!(txs.read(&mut read_buf).expect("unable to read"), 4);
        assert_eq!(bytes, read_buf);

        // Write to the socket
        let bytes = [0x14, 0x15, 0x16, 0x17];
        assert_eq!(txs.write(&bytes).expect("write failed"), 4);

        // Pump to read bytes from socket and write to the channel.
        assert!(exec.run_until_stalled(&mut fut).is_pending());

        // Read from the channel
        let mut buffer = zx::MessageBuf::new();
        txc.read(&mut buffer).expect("unable to read");
        assert_eq!(bytes, buffer.bytes());

        // Drop channel and expect the futures to return.
        txs.half_close().expect("should close");
        drop(txs);
        match exec.run_until_stalled(&mut fut) {
            Poll::Ready(Err(_)) => {}
            _ => {
                assert!(false, "still pending");
            }
        };
    }
}
