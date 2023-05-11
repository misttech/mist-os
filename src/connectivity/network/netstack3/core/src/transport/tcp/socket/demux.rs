// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines the entry point of TCP packets, by directing them into the correct
//! state machine.

use alloc::vec::Vec;
use assert_matches::assert_matches;
use core::{convert::TryFrom, fmt::Debug, num::NonZeroU16};
use log::trace;

use net_types::{
    ip::{IpAddress, IpVersionMarker},
    SpecifiedAddr,
};
use packet::{Buf, BufferMut, Serializer};
use packet_formats::{
    ip::IpProto,
    tcp::{
        TcpOptionsTooLongError, TcpParseArgs, TcpSegment, TcpSegmentBuilder,
        TcpSegmentBuilderWithOptions,
    },
};
use thiserror::Error;

use crate::{
    ip::{
        socket::{DefaultSendOptions, DeviceIpSocketHandler, MmsError},
        BufferIpTransportContext, BufferTransportIpContext, EitherDeviceId, IpLayerIpExt,
        TransportReceiveError,
    },
    socket::{
        address::{AddrVecIter, ConnAddr, ConnIpAddr, IpPortSpec, ListenerAddr},
        AddrVec,
    },
    transport::tcp::{
        buffer::SendPayload,
        segment::{Options, Segment},
        seqnum::WindowSize,
        socket::{
            do_send_inner, isn::IsnGenerator, Acceptor, Connection, ConnectionId,
            ConnectionStatusUpdate, Listener, ListenerAddrState, ListenerId, ListenerSharingState,
            MaybeClosedConnectionId, MaybeListener, MaybeListenerId, NonSyncContext, Sockets,
            SyncContext, TcpIpTransportContext, TimerId,
        },
        state::{BufferProvider, Closed, Initial, State},
        BufferSizes, ConnectionError, Control, Mss, SocketOptions,
    },
};

impl<C: NonSyncContext> BufferProvider<C::ReceiveBuffer, C::SendBuffer> for C {
    type ActiveOpen = C::ProvidedBuffers;

    type PassiveOpen = C::ReturnedBuffers;

    fn new_passive_open_buffers(
        buffer_sizes: BufferSizes,
    ) -> (C::ReceiveBuffer, C::SendBuffer, Self::PassiveOpen) {
        <C as NonSyncContext>::new_passive_open_buffers(buffer_sizes)
    }
}

impl<I, B, C, SC> BufferIpTransportContext<I, C, SC, B> for TcpIpTransportContext
where
    I: IpLayerIpExt,
    B: BufferMut,
    C: NonSyncContext
        + BufferProvider<
            C::ReceiveBuffer,
            C::SendBuffer,
            ActiveOpen = <C as NonSyncContext>::ProvidedBuffers,
            PassiveOpen = <C as NonSyncContext>::ReturnedBuffers,
        >,
    SC: SyncContext<I, C>,
{
    fn receive_ip_packet(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device: &SC::DeviceId,
        remote_ip: I::RecvSrcAddr,
        local_ip: SpecifiedAddr<I::Addr>,
        mut buffer: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        let remote_ip = match SpecifiedAddr::new(remote_ip.into()) {
            None => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: source address unspecified, dropping the packet");
                return Ok(());
            }
            Some(src_ip) => src_ip,
        };
        let packet =
            match buffer.parse_with::<_, TcpSegment<_>>(TcpParseArgs::new(*remote_ip, *local_ip)) {
                Ok(packet) => packet,
                Err(err) => {
                    // TODO(https://fxbug.dev/101993): Increment the counter.
                    trace!("tcp: failed parsing incoming packet {:?}", err);
                    return Ok(());
                }
            };
        let local_port = packet.dst_port();
        let remote_port = packet.src_port();
        let incoming = match Segment::try_from(packet) {
            Ok(segment) => segment,
            Err(err) => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: malformed segment {:?}", err);
                return Ok(());
            }
        };
        let now = ctx.now();

        let conn_addr =
            ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) };

        let addrs_to_search = AddrVecIter::<IpPortSpec<I, SC::WeakDeviceId>>::with_device(
            conn_addr.into(),
            sync_ctx.downgrade_device_id(device),
        );

        sync_ctx.with_ip_transport_ctx_isn_generator_and_tcp_sockets_mut(
            |ip_transport_ctx, isn, sockets| {
                handle_incoming_packet::<I, B, C, SC::IpTransportCtx<'_>>(
                    ctx,
                    ip_transport_ctx,
                    isn,
                    sockets,
                    conn_addr,
                    device,
                    addrs_to_search,
                    incoming,
                    now,
                )
            },
        );

        Ok(())
    }
}

fn handle_incoming_packet<I, B, C, SC>(
    ctx: &mut C,
    ip_transport_ctx: &mut SC,
    isn: &IsnGenerator<C::Instant>,
    sockets: &mut Sockets<I, SC::WeakDeviceId, C>,
    conn_addr: ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>,
    incoming_device: &SC::DeviceId,
    mut addrs_to_search: AddrVecIter<IpPortSpec<I, SC::WeakDeviceId>>,
    incoming: Segment<&[u8]>,
    now: C::Instant,
) where
    I: IpLayerIpExt,
    B: BufferMut,
    C: NonSyncContext
        + BufferProvider<
            C::ReceiveBuffer,
            C::SendBuffer,
            ActiveOpen = <C as NonSyncContext>::ProvidedBuffers,
            PassiveOpen = <C as NonSyncContext>::ReturnedBuffers,
        >,
    SC: BufferTransportIpContext<I, C, Buf<Vec<u8>>> + DeviceIpSocketHandler<I, C>,
{
    let any_usable_conn = addrs_to_search.any(|addr| {
        match addr {
            // Connections are always searched before listeners because they
            // are more specific.
            AddrVec::Conn(conn_addr) => {
                if let Some(conn_addr_state) = sockets.socketmap.conns().get_by_addr(&conn_addr) {
                    let conn_id = conn_addr_state.id();
                    match try_handle_incoming_for_connection::<I, SC, C, B>(
                        ip_transport_ctx,
                        ctx,
                        sockets,
                        conn_addr,
                        conn_id,
                        incoming,
                        now,
                    ) {
                        HandleIncomingSegmentDisposition::FoundSocket => true,
                        HandleIncomingSegmentDisposition::NoMatchingSocket => false,
                    }
                } else {
                    return false;
                }
            }
            AddrVec::Listen(listener_addr) => {
                // If we have a listener and the incoming segment is a SYN, we
                // allocate a new connection entry in the demuxer.
                // TODO(https://fxbug.dev/101992): Support SYN cookies.

                if let Some(addr_state) = sockets.socketmap.listeners().get_by_addr(&listener_addr)
                {
                    let id = match addr_state {
                        ListenerAddrState::ExclusiveListener(id) => id.clone().into(),
                        ListenerAddrState::Shared { listener: Some(id), bound: _ } => {
                            id.clone().into()
                        }
                        ListenerAddrState::ExclusiveBound(_)
                        | ListenerAddrState::Shared { listener: None, bound: _ } => return false,
                    };

                    match try_handle_incoming_for_listener::<I, SC, C, B>(
                        ip_transport_ctx,
                        ctx,
                        sockets,
                        isn,
                        id,
                        incoming,
                        conn_addr,
                        incoming_device,
                        now,
                    ) {
                        HandleIncomingSegmentDisposition::FoundSocket => true,
                        HandleIncomingSegmentDisposition::NoMatchingSocket => false,
                    }
                } else {
                    return false;
                }
            }
        }
    });

    let ConnIpAddr { local: (local_ip, _), remote: (remote_ip, _) } = conn_addr;
    if !any_usable_conn {
        // There is no existing TCP state, pretend it is closed
        // and generate a RST if needed.
        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
        // CLOSED is fictional because it represents the state when
        // there is no TCB, and therefore, no connection.
        if let Some(seg) = (Closed { reason: None::<Option<ConnectionError>> }.on_segment(incoming))
        {
            match ip_transport_ctx.new_ip_socket(
                ctx,
                None,
                Some(local_ip),
                remote_ip,
                IpProto::Tcp.into(),
                DefaultSendOptions,
            ) {
                Ok(ip_sock) => {
                    let body = tcp_serialize_segment(seg, conn_addr);
                    match ip_transport_ctx.send_ip_packet(ctx, &ip_sock, body, None) {
                        Ok(()) => {}
                        Err((body, err)) => {
                            // TODO(https://fxbug.dev/101993): Increment the counter.
                            trace!("tcp: failed to send ip packet {:?}: {:?}", body, err)
                        }
                    }
                }
                Err(err) => {
                    // TODO(https://fxbug.dev/101993): Increment the counter.
                    trace!("cannot construct an ip socket to respond RST: {:?}, ignoring", err);
                }
            }
        }
    }
}

enum HandleIncomingSegmentDisposition {
    FoundSocket,
    NoMatchingSocket,
}

/// Tries to handle the incoming segment by providing it to a connected socket.
///
/// Returns `true` if the segment was handled, `false` if a different socket
/// should be tried.
fn try_handle_incoming_for_connection<I, SC, C, B>(
    ip_transport_ctx: &mut SC,
    ctx: &mut C,
    sockets: &mut Sockets<I, SC::WeakDeviceId, C>,
    conn_addr: ConnAddr<I::Addr, SC::WeakDeviceId, NonZeroU16, NonZeroU16>,
    conn_id: MaybeClosedConnectionId<I>,
    incoming: Segment<&[u8]>,
    now: C::Instant,
) -> HandleIncomingSegmentDisposition
where
    I: IpLayerIpExt,
    B: BufferMut,
    C: NonSyncContext
        + BufferProvider<
            C::ReceiveBuffer,
            C::SendBuffer,
            ActiveOpen = <C as NonSyncContext>::ProvidedBuffers,
            PassiveOpen = <C as NonSyncContext>::ReturnedBuffers,
        >,
    SC: BufferTransportIpContext<I, C, Buf<Vec<u8>>> + DeviceIpSocketHandler<I, C>,
{
    let (conn, _, addr) = sockets
        .socketmap
        .conns_mut()
        .get_by_id_mut(&conn_id)
        .expect("inconsistent state: invalid connection id");

    let Connection { acceptor, state, ip_sock, defunct, socket_options, soft_error: _ } = conn;

    #[derive(Debug)]
    enum StateCategory {
        Connecting,
        Connected,
        Closed(Option<ConnectionError>),
    }
    impl StateCategory {
        fn new<I, R, S, A>(state: &State<I, R, S, A>) -> Self {
            match state {
                State::Established(_)
                | State::CloseWait(_)
                | State::LastAck(_)
                | State::FinWait1(_)
                | State::FinWait2(_)
                | State::Closing(_)
                | State::TimeWait(_) => StateCategory::Connected,
                State::Closed(Closed { reason }) => StateCategory::Closed(*reason),
                State::Listen(_) | State::SynRcvd(_) | State::SynSent(_) => {
                    StateCategory::Connecting
                }
            }
        }
    }

    let prev_state = StateCategory::new(state);

    // Send the reply to the segment immediately.
    let (reply, passive_open) = state.on_segment::<_, C>(incoming, now, socket_options, *defunct);

    let current_state = StateCategory::new(state);

    match current_state {
        StateCategory::Connecting => (),
        StateCategory::Closed(reason) => {
            if *defunct {
                // If the incoming segment caused the state machine to
                // enter Closed state, and the user has already promised
                // not to use the connection again, we can remove the
                // connection from the socketmap.
                assert_matches!(sockets.socketmap.conns_mut().remove(&conn_id), Some(_));
                let _: Option<_> = ctx.cancel_timer(TimerId::new::<I>(conn_id));
                return HandleIncomingSegmentDisposition::FoundSocket;
            }

            // If the socket does have an acceptor, it hasn't been pulled from
            // the accept queue of a listener and so its ID isn't yet known to
            // bindings. We only want to notify of connection status updates for
            // IDs that bindings is aware of.
            if acceptor.is_none() {
                match (prev_state, reason) {
                    (StateCategory::Connected | StateCategory::Connecting, err) => {
                        if let Some(err) = err {
                            let MaybeClosedConnectionId(id, marker) = conn_id;
                            let conn_id = ConnectionId(id, marker);
                            ctx.on_connection_status_change(
                                conn_id,
                                ConnectionStatusUpdate::Aborted(err),
                            )
                        }
                    }
                    (StateCategory::Closed(_), _) => {
                        // No change, no need to signal.
                    }
                }
            }
        }
        StateCategory::Connected => {
            match prev_state {
                StateCategory::Connected => {
                    // No change, no need to signal
                }
                StateCategory::Connecting => {
                    if acceptor.is_none() {
                        let MaybeClosedConnectionId(id, marker) = conn_id;
                        let conn_id = ConnectionId(id, marker);
                        ctx.on_connection_status_change(conn_id, ConnectionStatusUpdate::Connected)
                    }
                }
                StateCategory::Closed(_) => {
                    unreachable!("can't go from closed to established")
                }
            }
        }
    }

    if let Some(seg) = reply {
        let body = tcp_serialize_segment(seg, conn_addr.ip);
        match ip_transport_ctx.send_ip_packet(ctx, &ip_sock, body, None) {
            Ok(()) => {}
            Err((body, err)) => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: failed to send ip packet {:?}: {:?}", body, err)
            }
        }
    }

    // Send any enqueued data, if there is any.
    do_send_inner(conn_id, conn, addr, ip_transport_ctx, ctx);

    // Enqueue the connection to the associated listener
    // socket's accept queue.
    if let Some(passive_open) = passive_open {
        let acceptor_id = assert_matches!(conn, Connection {
            acceptor:Some(Acceptor::Pending(listener_id)),
            state: _,
            ip_sock: _,
            defunct: _,
            socket_options: _, soft_error: _
        } => {
            let listener_id = *listener_id;
            conn.acceptor = Some(Acceptor::Ready(listener_id));
            listener_id
        });
        let Listener { pending, ready, backlog: _, buffer_sizes: _, socket_options: _ } =
            sockets.get_listener_by_id_mut(acceptor_id).expect("orphaned acceptee");
        let pos = pending
            .iter()
            .position(|x| MaybeClosedConnectionId::from(*x) == conn_id)
            .expect("acceptee is not found in acceptor's pending queue");
        let conn = pending.swap_remove(pos);
        ready.push_back((conn, passive_open));
        ctx.on_waiting_connections_change(acceptor_id, ready.len());
    }

    // We found a valid connection for the segment.
    HandleIncomingSegmentDisposition::FoundSocket
}

/// Tries to handle an incoming segment by passing it to a listening socket.
///
/// Returns `true` if the segment was handled, otherwise `false`.
fn try_handle_incoming_for_listener<I, SC, C, B>(
    ip_transport_ctx: &mut SC,
    ctx: &mut C,
    sockets: &mut Sockets<I, SC::WeakDeviceId, C>,
    isn: &IsnGenerator<C::Instant>,
    listener_id: MaybeListenerId<I>,
    incoming: Segment<&[u8]>,
    incoming_addrs: ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>,
    incoming_device: &SC::DeviceId,
    now: C::Instant,
) -> HandleIncomingSegmentDisposition
where
    I: IpLayerIpExt,
    B: BufferMut,
    C: NonSyncContext
        + BufferProvider<
            C::ReceiveBuffer,
            C::SendBuffer,
            ActiveOpen = <C as NonSyncContext>::ProvidedBuffers,
            PassiveOpen = <C as NonSyncContext>::ReturnedBuffers,
        >,
    SC: BufferTransportIpContext<I, C, Buf<Vec<u8>>> + DeviceIpSocketHandler<I, C>,
{
    let socketmap = &mut sockets.socketmap;
    let (maybe_listener, sharing, listener_addr) =
        socketmap.listeners().get_by_id(&listener_id).expect("invalid listener_id");

    let ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) } =
        incoming_addrs;

    let Listener { pending, backlog, buffer_sizes, ready, socket_options } = match maybe_listener {
        MaybeListener::Bound(_bound) => {
            // If the socket is only bound, but not listening.
            return HandleIncomingSegmentDisposition::NoMatchingSocket;
        }
        MaybeListener::Listener(listener) => listener,
    };

    if pending.len() + ready.len() == backlog.get() {
        // TODO(https://fxbug.dev/101993): Increment the counter.
        trace!("incoming SYN dropped because of the full backlog of the listener");
        return HandleIncomingSegmentDisposition::FoundSocket;
    }

    let ListenerAddr { ip: _, device: bound_device } = listener_addr;
    // Ensure that if the remote address requires a zone, we propagate that to
    // the address for the connected socket.
    let bound_device = bound_device.as_ref();
    let bound_device = if crate::socket::must_have_zone(&remote_ip) {
        Some(bound_device.map_or(EitherDeviceId::Strong(incoming_device), EitherDeviceId::Weak))
    } else {
        bound_device.map(EitherDeviceId::Weak)
    };

    let bound_device = bound_device.as_ref().map(|d| d.as_ref());
    let ip_sock = match ip_transport_ctx.new_ip_socket(
        ctx,
        bound_device,
        Some(local_ip),
        remote_ip,
        IpProto::Tcp.into(),
        DefaultSendOptions,
    ) {
        Ok(ip_sock) => ip_sock,
        Err(err) => {
            // TODO(https://fxbug.dev/101993): Increment the counter.
            trace!("cannot construct an ip socket to the SYN originator: {:?}, ignoring", err);
            return HandleIncomingSegmentDisposition::NoMatchingSocket;
        }
    };

    let isn = isn.generate(
        now,
        (ip_sock.local_ip().clone(), local_port),
        (ip_sock.remote_ip().clone(), remote_port),
    );
    let device_mms = match ip_transport_ctx.get_mms(ctx, &ip_sock) {
        Ok(mms) => mms,
        Err(err) => {
            // If we cannot find a device or the device's MTU is too small,
            // there isn't much we can do here since sending a RST back is
            // impossible, we just need to silent drop the segment.
            log::error!("Cannot find a device with large enough MTU for the connection");
            match err {
                MmsError::NoDevice(_) | MmsError::MTUTooSmall(_) => {
                    return HandleIncomingSegmentDisposition::FoundSocket
                }
            }
        }
    };
    let Some(device_mss) = Mss::from_mms::<I>(device_mms) else {
        return HandleIncomingSegmentDisposition::FoundSocket
    };

    let mut state = State::Listen(Closed::<Initial>::listen(
        isn,
        buffer_sizes.clone(),
        device_mss,
        Mss::default::<I>(),
        socket_options.user_timeout,
    ));
    let reply = assert_matches!(
        state.on_segment::<_, C>(incoming, now, &SocketOptions::default(), false /* defunct */),
        (reply, None) => reply
    );
    if let Some(seg) = reply {
        let body = tcp_serialize_segment(seg, incoming_addrs);
        match ip_transport_ctx.send_ip_packet(ctx, &ip_sock, body, None) {
            Ok(()) => {}
            Err((body, err)) => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: failed to send ip packet {:?}: {:?}", body, err)
            }
        }
    }

    if matches!(state, State::SynRcvd(_)) {
        let poll_send_at = state.poll_send_at().expect("no retrans timer");
        let socket_options = socket_options.clone();
        let ListenerSharingState { sharing, listening: _ } = *sharing;
        let bound_device = bound_device.map(|d| d.as_weak_ref(ip_transport_ctx).into_owned());
        let MaybeListenerId(listener_index, _marker) = listener_id;
        let conn_id = socketmap
            .conns_mut()
            .try_insert(
                ConnAddr {
                    ip: ConnIpAddr {
                        local: (local_ip, local_port),
                        remote: (remote_ip, remote_port),
                    },
                    device: bound_device,
                },
                Connection {
                    acceptor: Some(Acceptor::Pending(ListenerId(
                        listener_index,
                        IpVersionMarker::default(),
                    ))),
                    state,
                    ip_sock,
                    defunct: false,
                    socket_options,
                    soft_error: None,
                },
                sharing,
            )
            .expect("failed to create a new connection")
            .id();
        assert_eq!(ctx.schedule_timer_instant(poll_send_at, TimerId::new::<I>(conn_id),), None);
        let (maybe_listener, _, _): (_, &ListenerSharingState, &ListenerAddr<_, _, _>) = sockets
            .socketmap
            .listeners_mut()
            .get_by_id_mut(&listener_id)
            .expect("the listener must still be active");

        match maybe_listener {
            MaybeListener::Bound(_bound) => {
                unreachable!("the listener must be active because we got here");
            }
            MaybeListener::Listener(listener) => {
                // This conversion is fine because
                // `conn_id` is newly created; No one
                // should have called close on it.
                let MaybeClosedConnectionId(id, marker) = conn_id;
                listener.pending.push(ConnectionId(id, marker));
            }
        }
    }

    // We found a valid listener for the segment.
    HandleIncomingSegmentDisposition::FoundSocket
}

#[derive(Error, Debug)]
#[error("Multiple mutually exclusive flags are set: syn: {syn}, fin: {fin}, rst: {rst}")]
pub(crate) struct MalformedFlags {
    syn: bool,
    fin: bool,
    rst: bool,
}

impl<'a> TryFrom<TcpSegment<&'a [u8]>> for Segment<&'a [u8]> {
    type Error = MalformedFlags;

    fn try_from(from: TcpSegment<&'a [u8]>) -> Result<Self, Self::Error> {
        if usize::from(from.syn()) + usize::from(from.fin()) + usize::from(from.rst()) > 1 {
            return Err(MalformedFlags { syn: from.syn(), fin: from.fin(), rst: from.rst() });
        }
        let syn = from.syn().then(|| Control::SYN);
        let fin = from.fin().then(|| Control::FIN);
        let rst = from.rst().then(|| Control::RST);
        let control = syn.or(fin).or(rst);
        let options = Options::from_iter(from.iter_options());
        let (to, discarded) = Segment::with_data_options(
            from.seq_num().into(),
            from.ack_num().map(Into::into),
            control,
            WindowSize::from_u16(from.window_size()),
            from.into_body(),
            options,
        );
        debug_assert_eq!(discarded, 0);
        Ok(to)
    }
}

pub(super) fn tcp_serialize_segment<'a, S, A>(
    segment: S,
    conn_addr: ConnIpAddr<A, NonZeroU16, NonZeroU16>,
) -> impl Serializer<Buffer = Buf<Vec<u8>>> + Debug + 'a
where
    S: Into<Segment<SendPayload<'a>>>,
    A: IpAddress,
{
    let Segment { seq, ack, wnd, contents, options } = segment.into();
    let ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) } = conn_addr;
    let mut builder = TcpSegmentBuilder::new(
        *local_ip,
        *remote_ip,
        local_port,
        remote_port,
        seq.into(),
        ack.map(Into::into),
        u16::try_from(u32::from(wnd)).unwrap_or(u16::MAX),
    );
    match contents.control() {
        None => {}
        Some(Control::SYN) => builder.syn(true),
        Some(Control::FIN) => builder.fin(true),
        Some(Control::RST) => builder.rst(true),
    }
    contents.data().encapsulate(
        TcpSegmentBuilderWithOptions::new(builder, options.iter()).unwrap_or_else(
            |TcpOptionsTooLongError| {
                panic!("Too many TCP options");
            },
        ),
    )
}

#[cfg(test)]
mod test {
    use ip_test_macro::ip_test;
    use net_types::ip::{Ip, Ipv4, Ipv6};
    use nonzero_ext::nonzero;
    use packet::ParseBuffer as _;
    use test_case::test_case;

    use crate::{
        testutil::TestIpExt,
        transport::tcp::{seqnum::SeqNum, Mss},
    };

    use super::*;

    const SEQ: SeqNum = SeqNum::new(12345);
    const ACK: SeqNum = SeqNum::new(67890);

    impl Segment<SendPayload<'static>> {
        const FAKE_DATA: &'static [u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 0];
        fn with_fake_data(split: bool) -> Self {
            let (segment, discarded) = Self::with_data(
                SEQ,
                Some(ACK),
                None,
                WindowSize::DEFAULT,
                if split {
                    let (first, second) = Self::FAKE_DATA.split_at(Self::FAKE_DATA.len() / 2);
                    SendPayload::Straddle(first, second)
                } else {
                    SendPayload::Contiguous(Self::FAKE_DATA)
                },
            );
            assert_eq!(discarded, 0);
            segment
        }
    }

    #[ip_test]
    #[test_case(Segment::syn(SEQ, WindowSize::DEFAULT, Options { mss: None }).into(), &[]; "syn")]
    #[test_case(Segment::syn(SEQ, WindowSize::DEFAULT, Options { mss: Some(Mss(nonzero_ext::nonzero!(1440 as u16))) }).into(), &[]; "syn with mss")]
    #[test_case(Segment::ack(SEQ, ACK, WindowSize::DEFAULT).into(), &[]; "ack")]
    #[test_case(Segment::with_fake_data(false), Segment::FAKE_DATA; "contiguous data")]
    #[test_case(Segment::with_fake_data(true), Segment::FAKE_DATA; "split data")]
    fn tcp_serialize_segment<I: Ip + TestIpExt>(
        segment: Segment<SendPayload<'_>>,
        expected_body: &[u8],
    ) {
        const SOURCE_PORT: NonZeroU16 = nonzero!(1111u16);
        const DEST_PORT: NonZeroU16 = nonzero!(2222u16);

        let options = segment.options;
        let serializer = super::tcp_serialize_segment(
            segment,
            ConnIpAddr {
                local: (I::FAKE_CONFIG.local_ip, SOURCE_PORT),
                remote: (I::FAKE_CONFIG.remote_ip, DEST_PORT),
            },
        );

        let mut serialized = serializer.serialize_vec_outer().unwrap().into_inner();
        let parsed_segment = serialized
            .parse_with::<_, TcpSegment<_>>(TcpParseArgs::new(
                *I::FAKE_CONFIG.remote_ip,
                *I::FAKE_CONFIG.local_ip,
            ))
            .expect("is valid segment");

        assert_eq!(parsed_segment.src_port(), SOURCE_PORT);
        assert_eq!(parsed_segment.dst_port(), DEST_PORT);
        assert_eq!(parsed_segment.seq_num(), u32::from(SEQ));
        assert_eq!(WindowSize::from_u16(parsed_segment.window_size()), WindowSize::DEFAULT);
        assert_eq!(options.iter().count(), parsed_segment.iter_options().count());
        for (orig, parsed) in options.iter().zip(parsed_segment.iter_options()) {
            assert_eq!(orig, parsed);
        }
        assert_eq!(parsed_segment.into_body(), expected_body);
    }
}
