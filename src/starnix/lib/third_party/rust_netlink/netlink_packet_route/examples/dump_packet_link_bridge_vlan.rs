// SPDX-License-Identifier: MIT

use netlink_packet_core::{
    NetlinkHeader, NetlinkMessage, NetlinkPayload, NLM_F_DUMP, NLM_F_REQUEST,
};
use netlink_packet_route::link::{LinkAttribute, LinkExtentMask, LinkMessage};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use netlink_sys::protocols::NETLINK_ROUTE;
use netlink_sys::{Socket, SocketAddr};

fn main() {
    let mut socket = Socket::new(NETLINK_ROUTE).unwrap();
    let _port_number = socket.bind_auto().unwrap().port_number();
    socket.connect(&SocketAddr::new(0, 0)).unwrap();

    let mut message = LinkMessage::default();
    message.header.interface_family = AddressFamily::Bridge;
    message.attributes.push(LinkAttribute::ExtMask(vec![LinkExtentMask::BrvlanCompressed]));

    let mut packet = NetlinkMessage::new(
        NetlinkHeader::default(),
        NetlinkPayload::from(RouteNetlinkMessage::GetLink(message)),
    );
    packet.header.flags = NLM_F_DUMP | NLM_F_REQUEST;
    packet.header.sequence_number = 1;
    packet.finalize();

    let mut buf = vec![0; packet.header.length as usize];

    // Before calling serialize, it is important to check that the buffer in
    // which we're emitting is big enough for the packet, other
    // `serialize()` panics.
    assert!(buf.len() == packet.buffer_len());
    packet.serialize(&mut buf[..]);

    println!(">>> {packet:?}");
    socket.send(&buf[..], 0).unwrap();

    let mut receive_buffer = vec![0; 4096];
    let mut offset = 0;

    // we set the NLM_F_DUMP flag so we expect a multipart rx_packet in
    // response.
    loop {
        let size = socket.recv(&mut &mut receive_buffer[..], 0).unwrap();

        loop {
            let bytes = &receive_buffer[offset..];
            // Note that we're parsing a NetlinkBuffer<&&[u8]>, NOT a
            // NetlinkBuffer<&[u8]> here. This is important because
            // Parseable<NetlinkMessage> is only implemented for
            // NetlinkBuffer<&'a T>, where T implements AsRef<[u8] + 'a. This is
            // not particularly user friendly, but this is a low
            // level library anyway.
            //
            // Note also that the same could be written more explicitely with:
            //
            // let rx_packet =
            //     <NetlinkBuffer<_> as
            // Parseable<NetlinkMessage>>::parse(NetlinkBuffer::new(&bytes))
            //         .unwrap();
            //
            let rx_packet: NetlinkMessage<RouteNetlinkMessage> =
                NetlinkMessage::deserialize(bytes).unwrap();

            println!("<<< {rx_packet:?}");

            if matches!(rx_packet.payload, NetlinkPayload::Done(_)) {
                println!("Done!");
                return;
            }

            offset += rx_packet.header.length as usize;
            if offset == size || rx_packet.header.length == 0 {
                offset = 0;
                break;
            }
        }
    }
}
