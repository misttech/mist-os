// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use bt_map::{Error as MapError, *};
use bt_obex::profile::{
    goep_l2cap_psm_attribute, parse_obex_search_result, GOEP_L2CAP_PSM_ATTRIBUTE,
};
use fidl_fuchsia_bluetooth_bredr as bredr;
use fuchsia_bluetooth::profile::*;
use fuchsia_bluetooth::types::Uuid;
use profile_client::ProfileClient;
use std::collections::HashSet;
use tracing::info;

const PROFILE_MAJOR_VERSION: u8 = 1;
const PROFILE_MINOR_VERSION: u8 = 4;

const MNS_SERVICE_NAME: &str = "MAP MNS-Fuchsia";

/// SDP Attribute ID for MapSupportedFeatures.
/// https://www.bluetooth.com/specifications/assigned-numbers/service-discovery
const ATTR_MAS_INSTANCE_ID: u16 = 0x0315;
const ATTR_SUPPORTED_MESSAGE_TYPES: u16 = 0x0316;
const ATTR_MAP_SUPPORTED_FEATURES: u16 = 0x0317;

/// Human readable attribute for the service name.
/// According to Core v5.3 vol. 3 part B section 5.1.8,
/// human readable attributes IDs are in the range 0x0100 to 0x01FF
/// and from Assigned Numbers section 5.2, the ID offset for ServiceName is 0x0000.
pub const ATTR_SERVICE_NAME: u16 = 0x100;

const FALLBACK_MAS_SERVICE_NAME: &str = "MAP MAS instance";

/// Represents a single Message Access Server (MAS) instance at a MSE peer.
/// A MSE peer may have one or more MAS instances.
/// MCE can access each MAS Instance by a dedicated OBEX connection.
/// See MAP v1.4.2 section 7.1.1 for deetails.
#[derive(Debug, PartialEq)]
pub struct MasConfig {
    instance_id: u8, // ID that identifies this MAS instance.
    name: String,
    supported_message_types: HashSet<MessageType>,
    connection: bredr::ConnectParameters,
    features: MapSupportedFeatures,
}

impl MasConfig {
    /// Cross checks the profile search result with the service definition for
    /// Message Access Service as listed in MAP v1.4.2 section 7.1.1.
    pub fn from_search_result(
        protocol: Vec<bredr::ProtocolDescriptor>,
        attributes: Vec<bredr::Attribute>,
    ) -> Result<Self, MapError> {
        // Ensure MAS service is advertised.
        let service_ids = find_service_classes(&attributes);
        if service_ids
            .iter()
            .find(|id| {
                id.number == bredr::ServiceClassProfileIdentifier::MessageAccessServer as u16
            })
            .is_none()
        {
            return Err(MapError::DoesNotExist(ServiceRecordItem::MasServiceClassId));
        }

        // Ensure MAP profile is advertised.
        let profile_desc = find_profile_descriptors(&attributes)
            .map_err(|_| MapError::DoesNotExist(ServiceRecordItem::MapProfileDescriptor))?;
        if profile_desc
            .iter()
            .find(|desc| {
                desc.profile_id == bredr::ServiceClassProfileIdentifier::MessageAccessProfile
            })
            .is_none()
        {
            return Err(MapError::DoesNotExist(ServiceRecordItem::MapProfileDescriptor));
        }

        let protocol = protocol.iter().map(Into::into).collect();
        let attributes = attributes.iter().map(Into::into).collect();

        // Ensure either L2CAP or RFCOMM connection is supported.
        let connection = parse_obex_search_result(&protocol, &attributes)
            .ok_or(MapError::NotGoepInteroperable)?;

        // Get information about this MAS instance.
        let id = attributes
            .iter()
            .find_map(|a| {
                if a.id != ATTR_MAS_INSTANCE_ID {
                    return None;
                }
                let DataElement::Uint8(raw_val) = a.element else {
                    return None;
                };
                Some(raw_val)
            })
            .ok_or(MapError::DoesNotExist(ServiceRecordItem::MasInstanceId))?;

        let name = attributes
            .iter()
            .find_map(|a| {
                // TODO(b/328074442): once getting languaged-based
                // attributes is supported, get the attributes through
                // that instead.
                if a.id != ATTR_SERVICE_NAME {
                    return None;
                }
                match &a.element {
                    DataElement::Str(bytes) => String::from_utf8(bytes.to_vec())
                        .ok()
                        .or(Some(FALLBACK_MAS_SERVICE_NAME.to_string())),
                    _ => Some(FALLBACK_MAS_SERVICE_NAME.to_string()),
                }
            })
            .ok_or(MapError::DoesNotExist(ServiceRecordItem::ServiceName))?;

        // Get supported times
        let supported_message_types = attributes
            .iter()
            .find_map(|a| {
                if a.id != ATTR_SUPPORTED_MESSAGE_TYPES {
                    return None;
                }
                let DataElement::Uint8(raw_val) = a.element else {
                    return None;
                };
                let supported: Vec<Result<MessageType, MapError>> =
                    MessageType::from_bits(raw_val).collect();
                let supported: HashSet<MessageType> =
                    supported.into_iter().filter_map(|r| r.ok()).collect();
                Some(supported)
            })
            .ok_or(MapError::DoesNotExist(ServiceRecordItem::SupportedMessageTypes))?;

        // We intersect the features supported by Sapphire and the features supported by
        // the peer device.
        let features = attributes
            .iter()
            .find_map(|a| {
                if a.id != ATTR_MAP_SUPPORTED_FEATURES {
                    return None;
                }
                let DataElement::Uint32(raw_val) = a.element else {
                    return None;
                };
                Some(MapSupportedFeatures::from_bits_truncate(raw_val))
            })
            .ok_or(MapError::DoesNotExist(ServiceRecordItem::MapSupportedFeatures))?;

        let config =
            MasConfig { instance_id: id, name, supported_message_types, connection, features };
        Ok(config)
    }
}

fn default_map_supported_features() -> MapSupportedFeatures {
    MapSupportedFeatures::NOTIFICATION_REGISTRATION
        | MapSupportedFeatures::NOTIFICATION
        | MapSupportedFeatures::BROWSING
        | MapSupportedFeatures::EXTENDED_EVENT_REPORT_1_1
        | MapSupportedFeatures::MESSAGES_LISTING_FORMAT_VERSION_1_1
        | MapSupportedFeatures::MAPSUPPORTEDFEATURES_IN_CONNECT_REQUEST
}

/// Service definition for Message Notification Service on the MCE device.
/// See MAP v.1.4.2, Section 7.1.2 for details.
fn build_mns_service_definition() -> ServiceDefinition {
    ServiceDefinition {
        service_class_uuids: vec![Uuid::new16(
            bredr::ServiceClassProfileIdentifier::MessageNotificationServer as u16,
        )
        .into()],
        protocol_descriptor_list: vec![
            ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::L2Cap, params: vec![] },
            ProtocolDescriptor {
                protocol: bredr::ProtocolIdentifier::Rfcomm,
                // Note: RFCOMM channel number is assigned by bt-rfcomm so we don't include it in
                // the parameter.
                params: vec![],
            },
            ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Obex, params: vec![] },
        ],
        information: vec![Information {
            language: "en".to_string(),
            // TODO(b/328115144): consider making this configurable
            // through structured configs.
            name: Some(MNS_SERVICE_NAME.to_string()),
            description: None,
            provider: None,
        }],
        profile_descriptors: vec![bredr::ProfileDescriptor {
            profile_id: bredr::ServiceClassProfileIdentifier::MessageAccessProfile,
            major_version: PROFILE_MAJOR_VERSION,
            minor_version: PROFILE_MINOR_VERSION,
        }],
        additional_attributes: vec![
            // Request a dynamic PSM to be assigned by the profile service.
            goep_l2cap_psm_attribute(Psm::new(bredr::PSM_DYNAMIC)),
            Attribute {
                id: ATTR_MAP_SUPPORTED_FEATURES,
                element: DataElement::Uint32(default_map_supported_features().bits()),
            },
        ],
        ..Default::default()
    }
}

pub fn connect_and_advertise(profile_svc: bredr::ProfileProxy) -> Result<ProfileClient, Error> {
    // Attributes to search for in SDP record for the Message Access Service on a MSE device.
    const SEARCH_ATTRIBUTES: [u16; 8] = [
        bredr::ATTR_SERVICE_CLASS_ID_LIST,
        bredr::ATTR_PROTOCOL_DESCRIPTOR_LIST,
        ATTR_SERVICE_NAME,
        bredr::ATTR_BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
        GOEP_L2CAP_PSM_ATTRIBUTE,
        ATTR_MAS_INSTANCE_ID,
        ATTR_SUPPORTED_MESSAGE_TYPES,
        ATTR_MAP_SUPPORTED_FEATURES,
    ];

    let service_defs = vec![(&build_mns_service_definition()).try_into()?];
    let channel_parameters = bredr::ChannelParameters {
        channel_mode: Some(bredr::ChannelMode::EnhancedRetransmission),
        ..Default::default()
    };

    // MCE device advertises the MNS on it and and searches for MAS on remote peers.
    let mut profile_client =
        ProfileClient::advertise(profile_svc.clone(), service_defs, channel_parameters)?;

    profile_client.add_search(
        bredr::ServiceClassProfileIdentifier::MessageAccessServer,
        &SEARCH_ATTRIBUTES,
    )?;

    info!("Registered service search & advertisement");

    Ok(profile_client)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns protocols with RFCOMM and OBEX protocols for testing purposes.
    fn test_protocols() -> Vec<bredr::ProtocolDescriptor> {
        vec![
            bredr::ProtocolDescriptor {
                protocol: bredr::ProtocolIdentifier::Rfcomm,
                params: vec![bredr::DataElement::Uint8(8)],
            },
            bredr::ProtocolDescriptor { protocol: bredr::ProtocolIdentifier::Obex, params: vec![] },
        ]
    }

    /// Returns attributes for testing purposes. Contains all the attributes for a MAS service record
    /// except for the GoepL2CapPsm attribute.
    fn test_attributes() -> Vec<bredr::Attribute> {
        vec![
            bredr::Attribute {
                id: bredr::ATTR_SERVICE_CLASS_ID_LIST,
                element: bredr::DataElement::Sequence(vec![Some(Box::new(
                    bredr::DataElement::Uuid(
                        Uuid::new16(
                            bredr::ServiceClassProfileIdentifier::MessageAccessServer as u16,
                        )
                        .into(),
                    ),
                ))]),
            },
            bredr::Attribute {
                id: bredr::ATTR_BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
                element: bredr::DataElement::Sequence(vec![Some(Box::new(
                    bredr::DataElement::Sequence(vec![
                        Some(Box::new(bredr::DataElement::Uuid(
                            Uuid::new16(
                                bredr::ServiceClassProfileIdentifier::MessageAccessProfile as u16,
                            )
                            .into(),
                        ))),
                        Some(Box::new(bredr::DataElement::Uint16(0x0104))),
                    ]),
                ))]),
            },
            bredr::Attribute {
                id: ATTR_SERVICE_NAME,
                element: bredr::DataElement::Str(vec![0x68, 0x69]),
            },
            bredr::Attribute { id: ATTR_MAS_INSTANCE_ID, element: bredr::DataElement::Uint8(1) },
            bredr::Attribute {
                id: ATTR_SUPPORTED_MESSAGE_TYPES,
                element: bredr::DataElement::Uint8(0x05), // email and sms cdma.
            },
            bredr::Attribute {
                id: ATTR_MAP_SUPPORTED_FEATURES,
                element: bredr::DataElement::Uint32(0x00080007),
            },
        ]
    }

    #[test]
    fn mas_config_from_search_result_rfcomm() {
        let config = MasConfig::from_search_result(test_protocols(), test_attributes())
            .expect("should succeed");

        match config.connection {
            bredr::ConnectParameters::L2cap(_) => panic!("should not be L2cap"),
            bredr::ConnectParameters::Rfcomm(chan) => assert_eq!(chan.channel, Some(8)),
        };

        assert!(config.features.contains(MapSupportedFeatures::NOTIFICATION_REGISTRATION));
        assert!(config.features.contains(MapSupportedFeatures::NOTIFICATION));
        assert!(config.features.contains(MapSupportedFeatures::BROWSING));
        assert!(config
            .features
            .contains(MapSupportedFeatures::MAPSUPPORTEDFEATURES_IN_CONNECT_REQUEST));

        assert_eq!(config.instance_id, 1);
        assert_eq!(config.name, "hi".to_string());

        assert_eq!(
            config.supported_message_types,
            HashSet::from([MessageType::Email, MessageType::SmsCdma])
        )
    }

    #[test]
    fn mas_config_from_search_result_l2cap() {
        let mut protocols = test_protocols();

        // Add L2CAP protocol.
        protocols.push(bredr::ProtocolDescriptor {
            protocol: bredr::ProtocolIdentifier::L2Cap,
            params: vec![],
        });

        let mut attributes = test_attributes();

        // Set service name attribute to an invalid value to test fall back name.
        for a in attributes.iter_mut() {
            if a.id == ATTR_SERVICE_NAME {
                a.element = bredr::DataElement::Uint8(0x00);
            }
        }

        // Add GOEP L2CAP Psm attribute to test GOEP interoperability.
        attributes.push(bredr::Attribute {
            id: GOEP_L2CAP_PSM_ATTRIBUTE,
            element: bredr::DataElement::Uint16(0x1007),
        });

        let config = MasConfig::from_search_result(protocols, attributes).expect("should succeed");

        match config.connection {
            bredr::ConnectParameters::L2cap(chan) => assert_eq!(chan.psm, Some(0x1007)),
            bredr::ConnectParameters::Rfcomm(_) => panic!("should not be Rfcomm"),
        };
        assert_eq!(config.name, FALLBACK_MAS_SERVICE_NAME.to_string());
    }

    #[test]
    fn mas_config_from_search_result_rfu_message_types() {
        let protocols = test_protocols();

        let mut attributes = test_attributes();

        // Set RFU bits in supported message types attribute.
        for a in attributes.iter_mut() {
            if a.id == ATTR_SUPPORTED_MESSAGE_TYPES {
                a.element = bredr::DataElement::Uint8(0b10000101); // email and sms cdma and bit 7.
            }
        }

        let config = MasConfig::from_search_result(protocols, attributes).expect("should succeed");

        assert_eq!(
            config.supported_message_types,
            HashSet::from([MessageType::Email, MessageType::SmsCdma])
        );
    }

    #[test]
    fn mas_config_from_search_result_fail() {
        // Missing obex.
        let _ = MasConfig::from_search_result(
            vec![bredr::ProtocolDescriptor {
                protocol: bredr::ProtocolIdentifier::Rfcomm,
                params: vec![bredr::DataElement::Uint8(8)],
            }],
            vec![
                bredr::Attribute {
                    id: bredr::ATTR_SERVICE_CLASS_ID_LIST,
                    element: bredr::DataElement::Sequence(vec![Some(Box::new(
                        bredr::DataElement::Uuid(
                            Uuid::new16(
                                bredr::ServiceClassProfileIdentifier::MessageAccessServer as u16,
                            )
                            .into(),
                        ),
                    ))]),
                },
                bredr::Attribute {
                    id: bredr::ATTR_BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
                    element: bredr::DataElement::Sequence(vec![Some(Box::new(
                        bredr::DataElement::Sequence(vec![
                            Some(Box::new(bredr::DataElement::Uuid(
                                Uuid::new16(
                                    bredr::ServiceClassProfileIdentifier::MessageAccessProfile
                                        as u16,
                                )
                                .into(),
                            ))),
                            Some(Box::new(bredr::DataElement::Uint16(0x0104))),
                        ]),
                    ))]),
                },
                bredr::Attribute {
                    id: ATTR_SERVICE_NAME,
                    element: bredr::DataElement::Str(vec![0x68, 0x69]),
                },
                bredr::Attribute {
                    id: ATTR_MAS_INSTANCE_ID,
                    element: bredr::DataElement::Uint8(1),
                },
                bredr::Attribute {
                    id: ATTR_SUPPORTED_MESSAGE_TYPES,
                    element: bredr::DataElement::Uint8(0x05), // email and sms cdma.
                },
                bredr::Attribute {
                    id: ATTR_MAP_SUPPORTED_FEATURES,
                    element: bredr::DataElement::Uint32(0x00080007),
                },
            ],
        )
        .expect_err("should fail");

        // Missing MAS instance ID.
        let _ = MasConfig::from_search_result(
            vec![
                bredr::ProtocolDescriptor {
                    protocol: bredr::ProtocolIdentifier::L2Cap,
                    params: vec![],
                },
                bredr::ProtocolDescriptor {
                    protocol: bredr::ProtocolIdentifier::Rfcomm,
                    params: vec![bredr::DataElement::Uint8(8)],
                },
                bredr::ProtocolDescriptor {
                    protocol: bredr::ProtocolIdentifier::Obex,
                    params: vec![],
                },
            ],
            vec![
                bredr::Attribute {
                    id: bredr::ATTR_SERVICE_CLASS_ID_LIST,
                    element: bredr::DataElement::Sequence(vec![Some(Box::new(
                        bredr::DataElement::Uuid(
                            Uuid::new16(
                                bredr::ServiceClassProfileIdentifier::MessageAccessServer as u16,
                            )
                            .into(),
                        ),
                    ))]),
                },
                bredr::Attribute {
                    id: bredr::ATTR_BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
                    element: bredr::DataElement::Sequence(vec![Some(Box::new(
                        bredr::DataElement::Sequence(vec![
                            Some(Box::new(bredr::DataElement::Uuid(
                                Uuid::new16(
                                    bredr::ServiceClassProfileIdentifier::MessageAccessProfile
                                        as u16,
                                )
                                .into(),
                            ))),
                            Some(Box::new(bredr::DataElement::Uint16(0x0104))),
                        ]),
                    ))]),
                },
                bredr::Attribute {
                    id: GOEP_L2CAP_PSM_ATTRIBUTE,
                    element: bredr::DataElement::Uint16(0x1007),
                },
                bredr::Attribute {
                    id: ATTR_SERVICE_NAME,
                    element: bredr::DataElement::Uint8(0x00),
                },
                bredr::Attribute {
                    id: ATTR_SUPPORTED_MESSAGE_TYPES,
                    element: bredr::DataElement::Uint8(0x05), // email and sms cdma.
                },
                bredr::Attribute {
                    id: ATTR_MAP_SUPPORTED_FEATURES,
                    element: bredr::DataElement::Uint32(0x00080007),
                },
            ],
        )
        .expect_err("should fail");
    }
}
