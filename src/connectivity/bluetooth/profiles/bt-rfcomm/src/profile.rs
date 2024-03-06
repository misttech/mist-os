// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use bt_rfcomm::profile::{is_rfcomm_protocol, server_channel_from_protocol};
use bt_rfcomm::ServerChannel;
use fuchsia_bluetooth::profile::{DataElement, Psm, ServiceDefinition};
use std::collections::HashSet;

/// Updates `service` with `server_channel` if the service requests RFCOMM.
///
/// Updates the primary protocol descriptor only. SDP records for profiles
/// usually have the RFCOMM descriptor in the primary protocol.
///
/// Returns `Ok` if the service was updated.
pub fn update_svc_def_with_server_channel(
    service: &mut ServiceDefinition,
    server_channel: ServerChannel,
) -> Result<(), Error> {
    // If the service definition is not requesting RFCOMM, there is no need to update
    // with the server channel.
    if !is_rfcomm_service_definition(&service) {
        return Err(format_err!("Non-RFCOMM service definition provided"));
    }

    for desc in service.protocol_descriptor_list.iter_mut() {
        if desc.protocol == fidl_fuchsia_bluetooth_bredr::ProtocolIdentifier::Rfcomm {
            desc.params = vec![DataElement::Uint8(server_channel.into())];
            break;
        }
    }
    Ok(())
}

/// Returns true if the provided `service` requests RFCOMM.
pub fn is_rfcomm_service_definition(service: &ServiceDefinition) -> bool {
    is_rfcomm_protocol(&service.protocol_descriptor_list)
}

/// Returns true if any of the `services` request RFCOMM.
pub fn service_definitions_request_rfcomm(services: &Vec<ServiceDefinition>) -> bool {
    services.iter().map(is_rfcomm_service_definition).fold(false, |acc, is_rfcomm| acc || is_rfcomm)
}

/// Returns the server channels specified in `services`. It's possible that
/// none of the `services` request a ServerChannel in which case the returned set
/// will be empty.
pub fn server_channels_from_service_definitions(
    services: &Vec<ServiceDefinition>,
) -> HashSet<ServerChannel> {
    services
        .iter()
        .filter_map(|def| server_channel_from_protocol(&def.protocol_descriptor_list))
        .collect()
}

/// Returns a set of PSMs specified by a list of `services`.
pub fn psms_from_service_definitions(services: &Vec<ServiceDefinition>) -> HashSet<Psm> {
    services.iter().fold(HashSet::new(), |mut psms, service| {
        psms.extend(&service.psm_set());
        psms
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use fidl_fuchsia_bluetooth_bredr as bredr;
    use fuchsia_bluetooth::profile::{Attribute, ProtocolDescriptor};

    #[test]
    fn update_empty_service_definition_is_error() {
        let server_channel = ServerChannel::try_from(10).unwrap();
        let mut def = ServiceDefinition::default();

        // Empty definition doesn't request RFCOMM - shouldn't be updated.
        let result = update_svc_def_with_server_channel(&mut def, server_channel);
        assert_matches!(result, Err(_));

        let expected = ServiceDefinition::default();
        assert_eq!(def, expected);
    }

    #[test]
    fn update_non_rfcomm_service_definition_is_error() {
        let server_channel = ServerChannel::try_from(8).unwrap();
        let mut def = ServiceDefinition {
            protocol_descriptor_list: vec![ProtocolDescriptor {
                protocol: bredr::ProtocolIdentifier::L2Cap,
                params: vec![],
            }],
            ..ServiceDefinition::default()
        };
        let expected = def.clone();

        // Only L2CAP definition cannot be updated with RFCOMM.
        let result = update_svc_def_with_server_channel(&mut def, server_channel);
        assert_matches!(result, Err(_));
        // The original `def` should be unchanged.
        assert_eq!(def, expected);
    }

    #[test]
    fn update_service_definition_with_rfcomm() {
        let server_channel = ServerChannel::try_from(10).unwrap();
        let mut def = ServiceDefinition {
            protocol_descriptor_list: vec![
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::L2Cap, params: vec![] },
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Rfcomm, params: vec![] },
            ],
            ..ServiceDefinition::default()
        };

        // Normal case - definition is requesting RFCOMM. It should be updated with the
        // server channel.
        let result = update_svc_def_with_server_channel(&mut def, server_channel);
        assert_matches!(result, Ok(_));

        let expected = ServiceDefinition {
            protocol_descriptor_list: vec![
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::L2Cap, params: vec![] },
                ProtocolDescriptor {
                    protocol: bredr::ProtocolIdentifier::Rfcomm,
                    params: vec![DataElement::Uint8(10)],
                },
            ],
            ..ServiceDefinition::default()
        };
        assert_eq!(def, expected);
    }

    #[test]
    fn update_obex_service_definition_with_rfcomm() {
        let server_channel = ServerChannel::try_from(12).unwrap();
        let mut def = ServiceDefinition {
            protocol_descriptor_list: vec![
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::L2Cap, params: vec![] },
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Rfcomm, params: vec![] },
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Obex, params: vec![] },
            ],
            ..ServiceDefinition::default()
        };

        // Definition is requesting RFCOMM, but OBEX is the "highest" protocol level, not RFCOMM.
        let result = update_svc_def_with_server_channel(&mut def, server_channel);
        assert_matches!(result, Ok(_));

        // We expect the RFCOMM descriptor to be updated and the OBEX descriptor should still be
        // preserved.
        let expected = ServiceDefinition {
            protocol_descriptor_list: vec![
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::L2Cap, params: vec![] },
                ProtocolDescriptor {
                    protocol: bredr::ProtocolIdentifier::Rfcomm,
                    params: vec![DataElement::Uint8(12)],
                },
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Obex, params: vec![] },
            ],
            ..ServiceDefinition::default()
        };
        assert_eq!(def, expected);
    }

    #[test]
    fn psm_from_service_definitions() {
        // Service 1 is only L2CAP.
        let def1 = ServiceDefinition {
            protocol_descriptor_list: vec![ProtocolDescriptor {
                protocol: bredr::ProtocolIdentifier::L2Cap,
                params: vec![DataElement::Uint16(21)],
            }],
            additional_protocol_descriptor_lists: vec![
                vec![ProtocolDescriptor {
                    protocol: bredr::ProtocolIdentifier::L2Cap,
                    params: vec![DataElement::Uint16(23)],
                }],
                vec![ProtocolDescriptor {
                    protocol: bredr::ProtocolIdentifier::Avdtp,
                    params: vec![DataElement::Uint16(0x0103)],
                }],
            ],
            ..ServiceDefinition::default()
        };
        // Service 2 is RFCOMM + L2CAP (OBEX).
        let def2 = ServiceDefinition {
            protocol_descriptor_list: vec![
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::L2Cap, params: vec![] },
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Rfcomm, params: vec![] },
                ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Obex, params: vec![] },
            ],
            additional_attributes: vec![Attribute {
                id: 0x0200,
                element: DataElement::Uint16(2000),
            }],
            ..ServiceDefinition::default()
        };

        let psms = psms_from_service_definitions(&vec![def1, def2]);

        // Expect to contain all of the PSMs that are specified in the record. Unallocated PSMs
        // (e.g. RFCOMM) aren't included.
        let expected_psms = HashSet::from([Psm::new(21), Psm::new(23), Psm::new(2000)]);
        assert_eq!(psms, expected_psms);
    }
}
