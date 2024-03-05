// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bitflags::bitflags;
use objects::ObexObjectError;
use packet_encoding::{codable_as_bitmask, decodable_enum};
use std::fmt;
use std::str::FromStr;

pub mod packets;

use thiserror::Error;

/// Errors that occur during the use of the MAP library.
#[non_exhaustive]
#[derive(Error, Debug)]
pub enum Error {
    #[error("Obex error: {:?}", .0)]
    Obex(ObexObjectError),

    #[error("Invalid message type")]
    InvalidMessageType,

    #[error("Service record item does not exist: {:?}", .0)]
    DoesNotExist(ServiceRecordItem),

    #[error("Service is not GOEP interoperable")]
    NotGoepInteroperable,
}

/// Service record item expected from MAP related SDP.
#[derive(Debug)]
pub enum ServiceRecordItem {
    MasServiceClassId,
    ObexProtocolDescriptor,
    MapProfileDescriptor,
    MasInstanceId,
    SupportedMessageTypes,
    MapSupportedFeatures,
    ServiceName,
}

bitflags! {
    /// See MAP v1.4.2 section 7.1 SDP Interoperability Requirements.
    /// According to MAP v1.4.2 section 6.3.1, the features represented
    /// in big-endian byte ordering.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MapSupportedFeatures: u32 {
        const NOTIFICATION_REGISTRATION                     = 0x00000001;
        const NOTIFICATION                                  = 0x00000002;
        const BROWSING                                      = 0x00000004;
        const UPLOADING                                     = 0x00000008;
        const DELETE                                        = 0x00000010;
        const INSTANCE_INFORMATION                          = 0x00000020;
        const EXTENDED_EVENT_REPORT_1_1                     = 0x00000040;
        const EVENT_REPORT_VERSION_1_2                      = 0x00000080;
        const MESSAGE_FORMAT_VERSION_1_1                    = 0x00000100;
        const MESSAGES_LISTING_FORMAT_VERSION_1_1           = 0x00000200;
        const PERSISTENT_MESSAGE_HANDLES                    = 0x00000400;
        const DATABASE_IDENTIFIER                           = 0x00000800;
        const FOLDER_VERSION_COUNTER                        = 0x00001000;
        const CONVERSATION_VERSION_COUNTER                  = 0x00002000;
        const PARTICIPANT_PRESENCE_CHANGE_NOTIFICATION      = 0x00004000;
        const PARTICIPANT_CHAT_STATE_CHANGE_NOTIFICATION    = 0x00008000;
        const PBAP_CONTACT_CROSS_REFERENCE                  = 0x00010000;
        const NOTIFICATION_FILTERING                        = 0x00020000;
        const UTC_OFFSET_TIMESTAMP_FORMAT                   = 0x00040000;
        const MAPSUPPORTEDFEATURES_IN_CONNECT_REQUEST       = 0x00080000;
        const CONVERSATION_LISTING                          = 0x00100000;
        const OWNER_STATUS                                  = 0x00200000;
        const MESSAGE_FORWARDING                            = 0x00400000;
    }
}

decodable_enum! {
    pub enum MessageType<u8, Error, InvalidMessageType> {
        Email = 0x1,
        SmsGsm = 0x2,
        SmsCdma = 0x4,
        Mms = 0x8,
        Im = 0x10,
        // Bits 5-7 are RFU.
    }
}

impl fmt::Display for MessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Email => write!(f, "EMAIL"),
            Self::SmsGsm => write!(f, "SMS_GSM"),
            Self::SmsCdma => write!(f, "SMS_CDMA"),
            Self::Mms => write!(f, "MMS"),
            Self::Im => write!(f, "IM"),
        }
    }
}

impl FromStr for MessageType {
    type Err = ObexObjectError;
    fn from_str(src: &str) -> Result<Self, Self::Err> {
        match src {
            "EMAIL" => Ok(Self::Email),
            "SMS_GSM" => Ok(Self::SmsGsm),
            "SMS_CDMA" => Ok(Self::SmsCdma),
            "MMS" => Ok(Self::Mms),
            "IM" => Ok(Self::Im),
            v => Err(ObexObjectError::invalid_data(v)),
        }
    }
}

codable_as_bitmask!(MessageType, u8, Error, InvalidMessageType);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn message_type() {
        let email_and_im = 0x11;

        let types_: HashSet<MessageType> = MessageType::from_bits(email_and_im)
            .collect::<Result<HashSet<_>, _>>()
            .expect("should not fail");

        assert_eq!(2, types_.len());

        let expected = [MessageType::Email, MessageType::Im].into_iter().collect();

        assert_eq!(types_, expected);

        let all = MessageType::VARIANTS;
        let value = MessageType::to_bits(all.iter()).expect("should work");
        assert_eq!(0x1F, value);
    }

    #[test]
    fn map_supported_features() {
        const NOTIFICATION_REG: u32 = 0x1;
        assert_eq!(
            MapSupportedFeatures::from_bits_truncate(NOTIFICATION_REG),
            MapSupportedFeatures::NOTIFICATION_REGISTRATION
        );

        const NOTIFICATION_REG_AND_OWNER_STATUS: u32 = 0x200001;
        let features = MapSupportedFeatures::from_bits_truncate(NOTIFICATION_REG_AND_OWNER_STATUS);
        assert!(features.contains(MapSupportedFeatures::NOTIFICATION_REGISTRATION));
        assert!(features.contains(MapSupportedFeatures::OWNER_STATUS));
        assert_eq!(
            features,
            MapSupportedFeatures::NOTIFICATION_REGISTRATION | MapSupportedFeatures::OWNER_STATUS
        );
    }
}
