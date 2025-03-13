// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fdf_component::{driver_register, Driver, DriverContext, Node};
use fidl::endpoints::{create_endpoints, ClientEnd};
use fidl_fuchsia_hardware_overnet::{
    self as overnet, CallbackMarker, CallbackProxy, DeviceRequestStream, UsbProxy,
};
use fuchsia_async::Socket;
use fuchsia_component::server::ServiceFs;
use futures::future::{select, Either};
use futures::io::{BufReader, ReadHalf, WriteHalf};
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, StreamExt, TryStreamExt};
use log::{debug, error, info, trace, warn};
use std::io::Error;
use std::pin::pin;
use zx::{SocketOpts, Status};

static MTU: usize = 1024;
static OVERNET_MAGIC: &[u8; 16] = b"OVERNET USB\xff\x00\xff\x00\xff";

struct OvernetUsbServiceDriver {
    /// The [`Node`] is our handle to the node we bound to. We need to keep this handle
    /// open to keep the node around.
    _node: Node,
}

driver_register!(OvernetUsbServiceDriver);

trait SocketCallback {
    async fn new_link(&mut self, socket: zx::Socket) -> Result<(), fidl::Error>;
}

impl SocketCallback for CallbackProxy {
    async fn new_link(&mut self, socket: zx::Socket) -> Result<(), fidl::Error> {
        CallbackProxy::new_link(self, socket).await
    }
}

/// Processes a connection to the underlying USB device through a datagram socket where each
/// packet received or sent corresponds to a USB bulk transfer buffer. It will call the callback
/// with a new link and close the old one whenever a magic reset packet is received from the host.
struct UsbConnection<P> {
    callback: P,
    usb_socket_reader: ReadHalf<Socket>,
    usb_socket_writer: WriteHalf<Socket>,
}

impl<P: SocketCallback> UsbConnection<P> {
    fn new(callback: P, usb_socket: zx::Socket) -> Self {
        assert!(
            usb_socket.info().unwrap().options.contains(SocketOpts::DATAGRAM),
            "USB socket must be a datagram socket"
        );
        let (usb_socket_reader, usb_socket_writer) = Socket::from_socket(usb_socket).split();
        Self { callback, usb_socket_reader, usb_socket_writer }
    }

    /// Waits for an [`OVERNET_MAGIC`] packet and sends the reply back, and then returns a
    /// fresh client socket for that overnet session.
    async fn next_socket(&mut self, mut found_magic: bool) -> Option<Socket> {
        let mut data = vec![0; OVERNET_MAGIC.len()];
        while !found_magic {
            let len = match self.usb_socket_reader.read(&mut data).await {
                Ok(0) => {
                    debug!("Usb socket closed");
                    return None;
                }
                Err(err) => {
                    error!("Unexpected error on usb socket: {err}");
                    return None;
                }
                Ok(len) => len,
            };
            debug!("Read {} bytes from usb socket", len);

            if data == OVERNET_MAGIC {
                found_magic = true;
            } else {
                warn!(
                    "Got {} bytes of garbage data before magic string on usb socket, discarding",
                    len
                );
            }
        }

        debug!("Read magic string, sending it back and setting up a new link");
        if let Err(err) = self.usb_socket_writer.write(OVERNET_MAGIC).await {
            error!("Error writing overnet magic string to the usb socket: {err:?}");
            return None;
        }
        let (next_client_socket, other_end) = zx::Socket::create_stream();
        if let Err(err) = self.callback.new_link(other_end).await {
            error!("Error sending socket end to overnet client: {err:?}");
            return None;
        }
        return Some(Socket::from_socket(next_client_socket));
    }

    async fn run(mut self) {
        let mut found_magic = false;
        loop {
            let Some(client_socket) = self.next_socket(found_magic).await else {
                info!("USB socket closed or failed");
                return;
            };
            // reset whether we found the magic string last time around or not.
            found_magic = false;

            let (client_reader, mut client_writer) = client_socket.split();
            let mut client_reader = BufReader::new(client_reader);
            let client_socket_copy =
                mtu_copy::<MTU>(&mut client_reader, &mut self.usb_socket_writer);
            let usb_socket_copy = magic_interrupt_copy::<MTU>(
                &mut found_magic,
                &mut self.usb_socket_reader,
                &mut client_writer,
            );
            let client_socket_copy = pin!(client_socket_copy);
            let usb_socket_copy = pin!(usb_socket_copy);
            let res = select(client_socket_copy, usb_socket_copy).await;
            match res {
                Either::Left((Err(err), _)) => {
                    warn!("Error on client to usb socket transfer: {err:?}");
                }
                Either::Left((Ok(_), _)) => {
                    debug!("client to usb socket closed normally");
                }
                Either::Right((Err(err), _)) => {
                    warn!("Error on usb to client socket transfer: {err:?}");
                }
                Either::Right((Ok(_), _)) => {
                    info!("usb to client socket closed normally");
                }
            }
        }
    }
}

/// Reads from `reader` in `MTU`-sized chunks and then writes them out to `writer`, ensuring that
/// we never write an item larger than `MTU` to it. This must be used with a datagram socket
/// or other writer where writes are guaranteed to transmit all data if the write returns success.
async fn mtu_copy<const MTU: usize>(
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<(), Error> {
    let mut data = [0; MTU];
    loop {
        let len = reader.read(&mut data).await?;
        trace!("Read {len} bytes from normal source");
        if len == 0 {
            break;
        }
        let data = &mut data[0..len];
        // we assert here because this must be used with a datagram-like socket.
        assert_eq!(writer.write(data).await?, len);
    }
    Ok(())
}

/// Reads from `reader` until it polls and finds the 'magic' string in the buffer. Until then
/// it writes all data received to the `writer`, leaving the magic string in the buffer if it
/// was found.
async fn magic_interrupt_copy<const MTU: usize>(
    found_magic: &mut bool,
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<(), Error> {
    let mut data = [0; MTU];
    loop {
        let len = reader.read(&mut data).await?;
        trace!("Read {len} bytes from interruptable source");
        if len == 0 {
            break;
        }
        let data = &mut data[0..len];
        if OVERNET_MAGIC == data {
            debug!("Found magic string, ending stream");
            *found_magic = true;
            break;
        }
        writer.write_all(data).await?;
    }
    Ok(())
}

/// Processes a stream of device connections from the parent driver, and for each one initiates a
/// [`UsbConnection`] process to handle individual connections to the host process.
struct OvernetService {
    request_stream: DeviceRequestStream,
    usb_device: UsbProxy,
}

impl OvernetService {
    async fn set_callback(&self, callback: ClientEnd<CallbackMarker>) -> Result<(), fidl::Error> {
        use overnet::CallbackRequest::*;
        let (usb_callback, usb_callback_server) = create_endpoints();
        self.usb_device.set_callback(usb_callback).await?;

        let mut usb_callback_server = usb_callback_server.into_stream();
        let callback = callback.into_proxy();
        while let Some(req) = usb_callback_server.try_next().await? {
            let NewLink { socket, responder } = req;
            responder.send()?;

            debug!("Received new socket from usb driver");
            UsbConnection::new(callback.clone(), socket).run().await;
        }
        Ok(())
    }

    async fn run(mut self) -> Result<(), fidl::Error> {
        use overnet::DeviceRequest::*;
        while let Some(req) = self.request_stream.try_next().await? {
            let SetCallback { callback, responder } = req;
            responder.send()?;

            self.set_callback(callback).await?;
        }
        Ok(())
    }
}

impl Driver for OvernetUsbServiceDriver {
    const NAME: &str = "overnet-usb-service";

    async fn start(mut context: DriverContext) -> Result<Self, Status> {
        let node = context.take_node()?;

        info!("Offering an overnet service in the outgoing directory");
        let mut outgoing = ServiceFs::new();
        let usb_device = get_usb_device(&context)?;
        outgoing.dir("svc").add_fidl_service_instance("default", move |i| {
            let overnet::ServiceRequest::Device(request_stream) = i;
            let usb_device = usb_device.clone();
            OvernetService { request_stream, usb_device }
        });

        context.serve_outgoing(&mut outgoing)?;

        fuchsia_async::Task::spawn(async move {
            outgoing
                .for_each(async |svc| {
                    if let Err(err) = svc.run().await {
                        error!("Error while servicing overnet client: {err:?}");
                    }
                })
                .await;
        })
        .detach();

        Ok(Self { _node: node })
    }

    async fn stop(&self) {}
}

fn get_usb_device(context: &DriverContext) -> Result<overnet::UsbProxy, Status> {
    let service_proxy = context.incoming.service_marker(overnet::UsbServiceMarker).connect()?;

    service_proxy.connect_to_device().map_err(|err| {
        error!("Error connecting to usb device proxy at driver startup: {err}");
        Status::INTERNAL
    })
}

#[cfg(test)]
mod tests {
    use fuchsia_async::Scope;
    use futures::channel::mpsc;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use Error;

    use super::*;

    #[derive(Clone, Default)]
    struct ExactPackets(VecDeque<Vec<u8>>);

    impl std::ops::Deref for ExactPackets {
        type Target = VecDeque<Vec<u8>>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl AsyncWrite for ExactPackets {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, Error>> {
            self.0.push_back(Vec::from(buf));
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncRead for ExactPackets {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<Result<usize, Error>> {
            let Some(first) = self.0.pop_front() else {
                return Poll::Ready(Ok(0));
            };
            assert!(buf.len() > first.len(), "buffer must be big enough to hold message");
            buf[..first.len()].copy_from_slice(&first);
            Poll::Ready(Ok(first.len()))
        }
    }

    #[fuchsia::test]
    async fn test_mtu_copy() {
        let inputs = [b'a'; 9000];
        let mut outputs = ExactPackets::default();
        mtu_copy::<1024>(&mut inputs.as_ref(), &mut outputs).await.unwrap();
        assert_eq!(
            Vec::from_iter(outputs.iter().map(|v| v.len())),
            vec![1024, 1024, 1024, 1024, 1024, 1024, 1024, 1024, 808],
            "output has been chunked into MTU-sized bits"
        )
    }

    #[fuchsia::test]
    async fn test_interrupt_copy() {
        let mut inputs =
            ExactPackets(VecDeque::from([vec![b'a'; 99], OVERNET_MAGIC.to_vec(), vec![b'b'; 99]]));
        let mut outputs = ExactPackets::default();
        let mut found_magic = false;
        magic_interrupt_copy::<1024>(&mut found_magic, &mut inputs, &mut outputs).await.unwrap();
        assert_eq!(
            *outputs,
            VecDeque::from([vec![b'a'; 99]]),
            "output contains everything up to the magic reset string"
        );
        assert_eq!(*inputs, VecDeque::from([vec![b'b'; 99]]), "input still contains the remainder");
        assert!(found_magic, "the magic reset string was found");
    }

    impl SocketCallback for mpsc::Sender<zx::Socket> {
        async fn new_link(&mut self, socket: zx::Socket) -> Result<(), fidl::Error> {
            futures::SinkExt::send(self, socket).await.unwrap();
            Ok(())
        }
    }

    #[fuchsia::test]
    async fn test_usb_connection() {
        let scope = Scope::new();
        //let mut inputs = ExactPackets(VecDeque::from([vec![b'a'; 10], OVERNET_MAGIC.to_vec(), vec![b'b'; 10], OVERNET_MAGIC.to_vec(), vec![b'c'; 10]]));
        let (usb_socket, other_end) = zx::Socket::create_datagram();
        let (mut usb_socket_reader, mut usb_socket_writer) =
            Socket::from_socket(usb_socket).split();
        let (link_tx, mut link_rx) = mpsc::channel(1);
        let connection = UsbConnection::new(link_tx, other_end);
        scope.spawn(connection.run());

        async fn expect_read(mut socket: (impl AsyncRead + Unpin), expected: impl AsRef<[u8]>) {
            let mut buf = vec![0; expected.as_ref().len()];
            socket.read_exact(&mut buf).await.unwrap();
            assert_eq!(expected.as_ref(), &*buf);
        }

        async fn expect_round_trip(
            mut write_sock: (impl AsyncWrite + Unpin),
            read_sock: (impl AsyncRead + Unpin),
            expected: impl AsRef<[u8]>,
        ) {
            write_sock.write_all(expected.as_ref()).await.unwrap();
            expect_read(read_sock, expected).await;
        }

        println!("writing some garbage that should get ignored");
        usb_socket_writer
            .write_all(b"this is garbage and should not affect anything")
            .await
            .unwrap();
        println!("testing first socket");
        expect_round_trip(&mut usb_socket_writer, &mut usb_socket_reader, OVERNET_MAGIC).await;
        let mut socket = Socket::from_socket(link_rx.next().await.unwrap());
        expect_round_trip(&mut socket, &mut usb_socket_reader, b"hello world!").await;
        expect_round_trip(&mut usb_socket_writer, socket, b"hello back!!").await;

        println!("testing second socket");
        expect_round_trip(&mut usb_socket_writer, &mut usb_socket_reader, OVERNET_MAGIC).await;
        let mut socket = Socket::from_socket(link_rx.next().await.unwrap());
        expect_round_trip(&mut socket, usb_socket_reader, b"hello new world!").await;
        expect_round_trip(usb_socket_writer, socket, b"hello back again!!").await;
        println!("hilo");

        println!("waiting for close");
        assert!(link_rx.next().await.is_none(), "expected other end to be closed");
        println!("waiting for task completion");
        scope.join().await;
    }
}
