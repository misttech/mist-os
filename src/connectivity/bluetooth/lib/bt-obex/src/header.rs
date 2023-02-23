// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_bluetooth::types::Uuid;
use packet_encoding::Decodable;

use crate::error::PacketError;

/// The OBEX Header Identifier (HI) identifies the type of OBEX packet.
///
/// The HI is a one-byte unsigned value and is split into the upper 2 bits and lower 6 bits. The
/// upper 2 bits indicate the header encoding and the lower 6 bits indicate the type of the
/// header.
/// Defined in OBEX 1.5 Section 2.1.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum HeaderIdentifier {
    /// Number of objects.
    Count = 0xC0,
    /// Name of the object (typically a file name).
    Name = 0x01,
    /// Type of object (e.g. text, html, ...)
    Type = 0x42,
    /// The length of the object in bytes.
    Length = 0xC3,
    /// Date/time stamp - ISO 8601. This representation is preferred.
    TimeIso8601 = 0x44,
    /// Date/time stamp - 4 byte representation.
    Time4Byte = 0xC4,
    /// Text description of the object.
    Description = 0x05,
    /// Name of the service that the operation is targeting.
    Target = 0x46,
    /// An HTTP 1.x header.
    Http = 0x47,
    /// A chunk of the object body.
    Body = 0x48,
    /// The final chunk of the object body.
    EndOfBody = 0x49,
    /// Identifies the OBEX application session.
    Who = 0x4A,
    /// An identifier associated with the OBEX connection - used for connection multiplexing.
    ConnectionId = 0xCB,
    /// Extended information about the OBEX connection.
    ApplicationParameters = 0x4C,
    /// Authentication digest challenge.
    AuthenticationChallenge = 0x4D,
    /// Authentication digest response.
    AuthenticationResponse = 0x4E,
    /// Indicates the creator of the object.
    CreatorId = 0xCF,
    /// Uniquely identifies the network client.
    WanUuid = 0x50,
    /// Class of an OBEX object,
    ObjectClass = 0x51,
    /// Parameters associated with the OBEX session.
    SessionParameters = 0x52,
    /// Sequence number included in each OBEX packet - used for reliability.
    SessionSequenceNumber = 0x93,
    /// Specifies the type of ACTION Operation.
    ActionId = 0x94,
    /// The destination for an object - used in certain ACTION Operations.
    DestName = 0x15,
    /// Bit mask for setting permissions.
    Permissions = 0xD6,
    /// Indicates that Single Response Mode (SRM) should be used.
    SingleResponseMode = 0x97,
    /// Specifies the parameters used during SRM.
    SingleResponseModeParameters = 0x98,
    // 0x30 to 0x3F, 0x70 to 0x7F, 0xB0 to 0xBF, 0xF0 to 0xFF, is user defined.
    User(u8),
    // 0x19 to 0x2F, 0x59 to 0x6F, 0x99 to 0xAF, 0xD9 to 0xEF, is RFA.
}

impl HeaderIdentifier {
    fn is_user(id: u8) -> bool {
        // The user-defined space is between 0x30 and 0x3f and includes all combinations of the
        // upper 2 bits of the `id`.
        let lower_6_bits = id & 0x3f;
        lower_6_bits >= 0x30 && lower_6_bits <= 0x3f
    }

    fn is_reserved(id: u8) -> bool {
        // The reserved space is between 0x19 and 0x2f and includes all combinations of the
        // upper 2 bits of the `id`.
        let lower_6_bits = id & 0x3f;
        lower_6_bits >= 0x19 && lower_6_bits <= 0x2f
    }
}

impl TryFrom<u8> for HeaderIdentifier {
    type Error = PacketError;

    fn try_from(src: u8) -> Result<Self, Self::Error> {
        match src {
            0xC0 => Ok(Self::Count),
            0x01 => Ok(Self::Name),
            0x42 => Ok(Self::Type),
            0xC3 => Ok(Self::Length),
            0x44 => Ok(Self::TimeIso8601),
            0xC4 => Ok(Self::Time4Byte),
            0x05 => Ok(Self::Description),
            0x46 => Ok(Self::Target),
            0x47 => Ok(Self::Http),
            0x48 => Ok(Self::Body),
            0x49 => Ok(Self::EndOfBody),
            0x4A => Ok(Self::Who),
            0xCB => Ok(Self::ConnectionId),
            0x4C => Ok(Self::ApplicationParameters),
            0x4D => Ok(Self::AuthenticationChallenge),
            0x4E => Ok(Self::AuthenticationResponse),
            0xCF => Ok(Self::CreatorId),
            0x50 => Ok(Self::WanUuid),
            0x51 => Ok(Self::ObjectClass),
            0x52 => Ok(Self::SessionParameters),
            0x93 => Ok(Self::SessionSequenceNumber),
            0x94 => Ok(Self::ActionId),
            0x15 => Ok(Self::DestName),
            0xD6 => Ok(Self::Permissions),
            0x97 => Ok(Self::SingleResponseMode),
            0x98 => Ok(Self::SingleResponseModeParameters),
            id if HeaderIdentifier::is_user(id) => Ok(Self::User(id)),
            id if HeaderIdentifier::is_reserved(id) => Err(Self::Error::Reserved),
            id => Err(Self::Error::Identifier(id)),
        }
    }
}

/// Represents a user-defined Header type.
// TODO(fxbug.dev/121500): This representation may change depending on what kind of user
// headers we expect.
pub struct UserDefinedHeader {
    /// The Header Identifier (HI) can be any value between 0x30 and 0x3f. See
    /// `HeaderIdentifier::User` for more details.
    identifier: u8,
    /// The user data.
    #[allow(unused)]
    value: Vec<u8>,
}

/// The building block of an OBEX object. A single OBEX object consists of one or more Headers.
pub enum Header {
    Count(u32),
    Name(String),
    Type(String),
    /// Number of bytes.
    Length(u32),
    /// Time represented as a String "YYYYMMDDTHHMMSSZ".
    TimeIso8601(String),
    Time4Byte(u32),
    Description(String),
    Target(Vec<u8>),
    Http(Vec<u8>),
    Body(Vec<u8>),
    EndOfBody(Vec<u8>),
    Who(Vec<u8>),
    ConnectionId(u32),
    ApplicationParameters(Vec<u8>),
    AuthenticationChallenge(Vec<u8>),
    AuthenticationResponse(Vec<u8>),
    CreatorId(u32),
    WanUuid(Uuid),
    ObjectClass(Vec<u8>),
    SessionParameters(Vec<u8>),
    SessionSequenceNumber(u8),
    ActionId(u8),
    DestName(String),
    /// 4-byte bit mask.
    Permissions(u32),
    SingleResponseMode(u8),
    /// 1-byte quantity containing
    SingleResponseModeParameters(u8),
    /// User defined Header type.
    User(UserDefinedHeader),
}

impl Header {
    pub fn identifier(&self) -> HeaderIdentifier {
        match &self {
            Self::Count(_) => HeaderIdentifier::Count,
            Self::Name(_) => HeaderIdentifier::Name,
            Self::Type(_) => HeaderIdentifier::Type,
            Self::Length(_) => HeaderIdentifier::Length,
            Self::TimeIso8601(_) => HeaderIdentifier::TimeIso8601,
            Self::Time4Byte(_) => HeaderIdentifier::Time4Byte,
            Self::Description(_) => HeaderIdentifier::Description,
            Self::Target(_) => HeaderIdentifier::Target,
            Self::Http(_) => HeaderIdentifier::Http,
            Self::Body(_) => HeaderIdentifier::Body,
            Self::EndOfBody(_) => HeaderIdentifier::EndOfBody,
            Self::Who(_) => HeaderIdentifier::Who,
            Self::ConnectionId(_) => HeaderIdentifier::ConnectionId,
            Self::ApplicationParameters(_) => HeaderIdentifier::ApplicationParameters,
            Self::AuthenticationChallenge(_) => HeaderIdentifier::AuthenticationChallenge,
            Self::AuthenticationResponse(_) => HeaderIdentifier::AuthenticationResponse,
            Self::CreatorId(_) => HeaderIdentifier::CreatorId,
            Self::WanUuid(_) => HeaderIdentifier::WanUuid,
            Self::ObjectClass(_) => HeaderIdentifier::ObjectClass,
            Self::SessionParameters(_) => HeaderIdentifier::SessionParameters,
            Self::SessionSequenceNumber(_) => HeaderIdentifier::SessionSequenceNumber,
            Self::ActionId(_) => HeaderIdentifier::ActionId,
            Self::DestName(_) => HeaderIdentifier::DestName,
            Self::Permissions(_) => HeaderIdentifier::Permissions,
            Self::SingleResponseMode(_) => HeaderIdentifier::SingleResponseMode,
            Self::SingleResponseModeParameters(_) => HeaderIdentifier::SingleResponseModeParameters,
            Self::User(UserDefinedHeader { identifier, .. }) => HeaderIdentifier::User(*identifier),
        }
    }
}

impl Decodable for Header {
    type Error = PacketError;

    fn decode(buf: &[u8]) -> Result<Self, Self::Error> {
        if buf.len() < 1 {
            return Err(PacketError::BufferTooSmall);
        }

        let _id = HeaderIdentifier::try_from(buf[0])?;
        todo!("TODO(fxbug.dev/120012): Finish parsing the entire Header packet.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    #[fuchsia::test]
    fn is_user_id() {
        let ids = [0x00, 0x10, 0x29, 0x40, 0x80, 0xc0];
        for id in ids {
            assert!(!HeaderIdentifier::is_user(id));
        }

        let ids = [0x30, 0x3f, 0x71, 0x7f, 0xb5, 0xbf, 0xf0, 0xff];
        for id in ids {
            assert!(HeaderIdentifier::is_user(id));
        }
    }

    #[fuchsia::test]
    fn is_reserved_id() {
        let ids = [0x00, 0x10, 0x30, 0x70, 0xb0, 0xf0];
        for id in ids {
            assert!(!HeaderIdentifier::is_reserved(id));
        }

        let ids = [0x19, 0x2f, 0x60, 0x6f, 0x99, 0xae, 0xd9, 0xef];
        for id in ids {
            assert!(HeaderIdentifier::is_reserved(id));
        }
    }

    #[fuchsia::test]
    fn valid_header_id_parsed_ok() {
        let valid = 0x15;
        let result = HeaderIdentifier::try_from(valid);
        assert_matches!(result, Ok(HeaderIdentifier::DestName));
    }

    #[fuchsia::test]
    fn user_header_id_is_ok() {
        let user_header_id_raw = 0x33;
        let result = HeaderIdentifier::try_from(user_header_id_raw);
        assert_matches!(result, Ok(HeaderIdentifier::User(_)));
    }

    #[fuchsia::test]
    fn rfa_header_id_is_reserved_error() {
        let rfa_header_id_raw = 0x20;
        let result = HeaderIdentifier::try_from(rfa_header_id_raw);
        assert_matches!(result, Err(PacketError::Reserved));
    }

    #[fuchsia::test]
    fn unknown_header_id_is_error() {
        // The lower 6 bits of this represent the Length Header. However, the upper 2 bits are
        // incorrect - therefore the Header ID is considered invalid.
        let unknown_header_id_raw = 0x03;
        let result = HeaderIdentifier::try_from(unknown_header_id_raw);
        assert_matches!(result, Err(PacketError::Identifier(_)));
    }
}
