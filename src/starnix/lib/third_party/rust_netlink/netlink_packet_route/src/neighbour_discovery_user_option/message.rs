// SPDX-License-Identifier: MIT

use netlink_packet_utils::traits::{Emitable, Parseable};
use std::convert::TryFrom as _;

use super::buffer::{
    NeighbourDiscoveryUserOptionMessageBuffer, NEIGHBOUR_DISCOVERY_USER_OPTION_HEADER_LEN,
};
use super::header::NeighbourDiscoveryUserOptionHeader;
use super::nla::Nla;
use super::NeighbourDiscoveryUserOptionError;

#[derive(Debug, PartialEq, Eq, Clone)]
#[non_exhaustive]
pub struct NeighbourDiscoveryUserOptionMessage {
    /// The header of the ND_USEROPT message.
    pub header: NeighbourDiscoveryUserOptionHeader,

    /// The body of the NDP option as it was on the wire.
    pub option_body: Vec<u8>,

    pub attributes: Vec<Nla>,
}

impl NeighbourDiscoveryUserOptionMessage {
    pub fn new(
        header: NeighbourDiscoveryUserOptionHeader,
        option_body: Vec<u8>,
        attributes: Vec<Nla>,
    ) -> Self {
        Self { header, option_body, attributes }
    }
}

impl Emitable for NeighbourDiscoveryUserOptionMessage {
    fn buffer_len(&self) -> usize {
        NEIGHBOUR_DISCOVERY_USER_OPTION_HEADER_LEN
            + self.option_body.len()
            + self.attributes.as_slice().buffer_len()
    }

    fn emit(&self, buffer: &mut [u8]) {
        let Self {
            header: NeighbourDiscoveryUserOptionHeader { interface_index, icmp_type },
            option_body,
            attributes,
        } = self;

        let mut packet = NeighbourDiscoveryUserOptionMessageBuffer::new(buffer);

        packet.set_address_family(icmp_type.family().into());

        let payload = packet.payload_mut();
        payload[..option_body.len()].copy_from_slice(&option_body[..]);
        attributes.as_slice().emit(&mut payload[option_body.len()..]);

        packet.set_options_length(
            u16::try_from(option_body.len())
                .expect("neighbor discovery options length doesn't fit in u16"),
        );
        packet.set_interface_index(*interface_index);

        let (icmp_type, icmp_code) = icmp_type.into_type_and_code();
        packet.set_icmp_type(icmp_type);
        packet.set_icmp_code(icmp_code);
    }
}

impl<'a, T: AsRef<[u8]> + 'a> Parseable<NeighbourDiscoveryUserOptionMessageBuffer<&'a T>>
    for NeighbourDiscoveryUserOptionMessage
{
    type Error = NeighbourDiscoveryUserOptionError;

    fn parse(
        buf: &NeighbourDiscoveryUserOptionMessageBuffer<&'a T>,
    ) -> Result<Self, NeighbourDiscoveryUserOptionError> {
        let header = NeighbourDiscoveryUserOptionHeader::parse(buf)
            .map_err(NeighbourDiscoveryUserOptionError::InvalidHeader)?;

        let mut nlas = Vec::new();
        for nla_buf in buf.nlas() {
            nlas.push(
                Nla::parse(&nla_buf.map_err(NeighbourDiscoveryUserOptionError::InvalidNla)?)
                    .map_err(NeighbourDiscoveryUserOptionError::InvalidNla)?,
            );
        }

        Ok(NeighbourDiscoveryUserOptionMessage {
            header,
            option_body: buf.option_body().to_vec(),
            attributes: nlas,
        })
    }
}
