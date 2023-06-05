// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bt_rfcomm::profile::{rfcomm_connect_parameters, server_channel_from_protocol};
use bt_rfcomm::ServerChannel;
use fidl_fuchsia_bluetooth_bredr as bredr;
use fuchsia_bluetooth::profile::{
    l2cap_connect_parameters, psm_from_protocol, Attribute, DataElement, ProtocolDescriptor, Psm,
};

/// The Attribute ID associated with the GoepL2capPsm attribute.
/// Defined in https://www.bluetooth.com/specifications/assigned-numbers/service-discovery
const GOEP_L2CAP_PSM_ATTRIBUTE: u16 = 0x0200;

/// Returns true if the provided `protocol` is OBEX.
///
/// Protocols are generally specified as a list of protocol descriptors which are ordered from
/// lowest level (typically L2CAP) to highest.
pub fn is_obex_protocol(protocol: &Vec<ProtocolDescriptor>) -> bool {
    protocol.iter().any(|descriptor| descriptor.protocol == bredr::ProtocolIdentifier::Obex)
}

/// Returns the protocol for an OBEX service for the provided L2CAP `psm`.
pub fn obex_protocol_l2cap(psm: Psm) -> Vec<ProtocolDescriptor> {
    vec![
        ProtocolDescriptor {
            protocol: bredr::ProtocolIdentifier::L2Cap,
            params: vec![DataElement::Uint16(psm.into())],
        },
        ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Obex, params: vec![] },
    ]
}

/// Returns the protocol for an OBEX service for the provided RFCOMM `channel` number.
pub fn obex_protocol_rfcomm(channel: ServerChannel) -> Vec<ProtocolDescriptor> {
    vec![
        ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::L2Cap, params: vec![] },
        ProtocolDescriptor {
            protocol: bredr::ProtocolIdentifier::Rfcomm,
            params: vec![DataElement::Uint8(channel.into())],
        },
        ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Obex, params: vec![] },
    ]
}

/// Returns the GoepL2capPsm attribute for the provided `psm`.
pub fn goep_l2cap_psm_attribute(psm: Psm) -> Attribute {
    Attribute { id: GOEP_L2CAP_PSM_ATTRIBUTE, element: DataElement::Uint16(psm.into()) }
}

/// Attempts to parse and return the PSM from the `attribute`. Returns the L2CAP PSM on success,
/// None otherwise.
fn parse_goep_l2cap_psm_attribute(attribute: &Attribute) -> Option<Psm> {
    if attribute.id != GOEP_L2CAP_PSM_ATTRIBUTE {
        return None;
    }

    if let DataElement::Uint16(psm) = attribute.element {
        Some(Psm::new(psm))
    } else {
        None
    }
}

/// Attempts to parse a peer's service advertisement into ConnectParameters containing the L2CAP
/// PSM or RFCOMM ServerChannel associated with the service.
/// Builds and returns the parameters on success, None otherwise.
pub fn parse_obex_search_result(
    protocol: &Vec<ProtocolDescriptor>,
    attributes: &Vec<Attribute>,
) -> Option<bredr::ConnectParameters> {
    if !is_obex_protocol(protocol) {
        return None;
    }

    // The GoepL2capPsm attribute is included when the peer supports both RFCOMM and L2CAP.
    // Prefer L2CAP if both are supported.
    if let Some(l2cap_psm) = attributes.iter().find_map(parse_goep_l2cap_psm_attribute) {
        return Some(l2cap_connect_parameters(l2cap_psm));
    }

    // Otherwise the service supports only one of L2CAP or RFCOMM.
    // Try L2CAP first.
    if let Some(psm) = psm_from_protocol(protocol) {
        return Some(l2cap_connect_parameters(psm));
    }

    // Otherwise, it's RFCOMM.
    server_channel_from_protocol(protocol).map(|sc| rfcomm_connect_parameters(sc))
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;

    #[test]
    fn parse_invalid_goep_attribute_is_none() {
        // Different data type than the expected u16 PSM.
        let attribute = Attribute { id: GOEP_L2CAP_PSM_ATTRIBUTE, element: DataElement::Uint8(5) };
        assert_matches!(parse_goep_l2cap_psm_attribute(&attribute), None);

        // Non-GOEP attribute.
        let attribute = Attribute {
            id: 0x3333, // Random attribute ID
            element: DataElement::Uint16(5),
        };
        assert_matches!(parse_goep_l2cap_psm_attribute(&attribute), None);
    }

    #[test]
    fn parse_goep_attribute_success() {
        let attribute =
            Attribute { id: GOEP_L2CAP_PSM_ATTRIBUTE, element: DataElement::Uint16(45) };
        assert_eq!(parse_goep_l2cap_psm_attribute(&attribute), Some(Psm::new(45)));
    }

    #[test]
    fn parse_invalid_search_result_is_none() {
        // A protocol with OBEX but no L2CAP or RFCOMM transport.
        let protocol =
            vec![ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Obex, params: vec![] }];
        assert_matches!(parse_obex_search_result(&protocol, &vec![]), None);
    }

    #[test]
    fn parse_non_obex_search_result_is_none() {
        // A protocol with just L2CAP - no OBEX.
        let protocol = vec![ProtocolDescriptor {
            protocol: bredr::ProtocolIdentifier::L2Cap,
            params: vec![DataElement::Uint16(27)],
        }];
        let attributes = vec![goep_l2cap_psm_attribute(Psm::new(55))];
        // Even though the search result contains the GoepL2capPsm, it should not be returned
        // because the protocol is not OBEX.
        assert_matches!(parse_obex_search_result(&protocol, &attributes), None);
    }

    #[test]
    fn parse_obex_search_result_with_l2cap() {
        let l2cap_protocol = obex_protocol_l2cap(Psm::new(59));
        let expected = bredr::ConnectParameters::L2cap(bredr::L2capParameters {
            psm: Some(59),
            ..bredr::L2capParameters::default()
        });
        let result =
            parse_obex_search_result(&l2cap_protocol, &vec![]).expect("valid search result");
        assert_eq!(result, expected);
    }

    #[test]
    fn parse_obex_search_result_with_rfcomm() {
        let server_channel = 8.try_into().unwrap();
        let rfcomm_protocol = obex_protocol_rfcomm(server_channel);
        let expected = bredr::ConnectParameters::Rfcomm(bredr::RfcommParameters {
            channel: Some(8),
            ..bredr::RfcommParameters::default()
        });
        let result =
            parse_obex_search_result(&rfcomm_protocol, &vec![]).expect("valid search result");
        assert_eq!(result, expected);
    }

    #[test]
    fn parse_obex_search_result_with_l2cap_and_rfcomm() {
        let server_channel = 7.try_into().unwrap();
        let attributes = vec![
            Attribute {
                id: 0x33, // Random attribute
                element: DataElement::Uint8(5),
            },
            goep_l2cap_psm_attribute(Psm::new(55)),
        ];
        // Expected should be the L2CAP PSM.
        let expected = bredr::ConnectParameters::L2cap(bredr::L2capParameters {
            psm: Some(55),
            ..bredr::L2capParameters::default()
        });
        let result = parse_obex_search_result(&obex_protocol_rfcomm(server_channel), &attributes)
            .expect("valid search result");
        assert_eq!(result, expected);
    }
}
