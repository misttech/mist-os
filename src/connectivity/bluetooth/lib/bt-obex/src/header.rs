// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_bluetooth::types::Uuid;
use packet_encoding::{decodable_enum, Decodable};
use tracing::trace;

use crate::error::PacketError;

decodable_enum! {
    /// The Header Encoding is the upper 2 bits of the Header Identifier (HI) and describes the type
    /// of payload included in the Header.
    /// Defined in OBEX 1.5 Section 2.1.
    enum HeaderEncoding<u8, PacketError, HeaderEncoding> {
        /// A Header with null terminated Unicode text. The text is encoded in UTF-16 format. The
        /// text length is encoded as a two byte unsigned integer.
        Text = 0x00,
        /// A Header with a byte sequence. The sequence length is encoded as a two byte unsigned
        /// integer.
        Bytes = 0x40,
        /// A Header with a 1-byte payload.
        OneByte = 0x80,
        /// A Header with a 4-byte payload.
        FourBytes = 0xC0,
    }
}

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

    fn encoding(&self) -> HeaderEncoding {
        let id_raw: u8 = self.into();
        // The encoding is the upper 2 bits of the HeaderIdentifier.
        HeaderEncoding::try_from(id_raw & 0xc0).expect("valid Header encoding")
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

impl Into<u8> for &HeaderIdentifier {
    fn into(self) -> u8 {
        match &self {
            HeaderIdentifier::Count => 0xC0,
            HeaderIdentifier::Name => 0x01,
            HeaderIdentifier::Type => 0x42,
            HeaderIdentifier::Length => 0xC3,
            HeaderIdentifier::TimeIso8601 => 0x44,
            HeaderIdentifier::Time4Byte => 0xC4,
            HeaderIdentifier::Description => 0x05,
            HeaderIdentifier::Target => 0x46,
            HeaderIdentifier::Http => 0x47,
            HeaderIdentifier::Body => 0x48,
            HeaderIdentifier::EndOfBody => 0x49,
            HeaderIdentifier::Who => 0x4A,
            HeaderIdentifier::ConnectionId => 0xCB,
            HeaderIdentifier::ApplicationParameters => 0x4C,
            HeaderIdentifier::AuthenticationChallenge => 0x4D,
            HeaderIdentifier::AuthenticationResponse => 0x4E,
            HeaderIdentifier::CreatorId => 0xCF,
            HeaderIdentifier::WanUuid => 0x50,
            HeaderIdentifier::ObjectClass => 0x51,
            HeaderIdentifier::SessionParameters => 0x52,
            HeaderIdentifier::SessionSequenceNumber => 0x93,
            HeaderIdentifier::ActionId => 0x94,
            HeaderIdentifier::DestName => 0x15,
            HeaderIdentifier::Permissions => 0xD6,
            HeaderIdentifier::SingleResponseMode => 0x97,
            HeaderIdentifier::SingleResponseModeParameters => 0x98,
            HeaderIdentifier::User(id) => *id,
        }
    }
}

/// Represents a user-defined Header type.
// TODO(fxbug.dev/121500): This representation may change depending on what kind of user
// headers we expect.
#[derive(Debug, PartialEq)]
pub struct UserDefinedHeader {
    /// The Header Identifier (HI) can be any value between 0x30 and 0x3f. See
    /// `HeaderIdentifier::User` for more details.
    identifier: u8,
    /// The user data.
    #[allow(unused)]
    value: Vec<u8>,
}

/// The building block of an OBEX object. A single OBEX object consists of one or more Headers.
#[derive(Debug, PartialEq)]
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
    /// The minimal Header contains at least a 1-byte identifier.
    const MIN_HEADER_LENGTH_BYTES: usize = 1;

    /// A Unicode or Byte Sequence Header must be at least 3 bytes - 1 byte for the HI and 2 bytes
    /// for the encoded data length.
    const MIN_UNICODE_OR_BYTE_SEQ_LENGTH_BYTES: usize = 3;

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
        // The buffer should contain at least the Header Identifier.
        if buf.len() < Self::MIN_HEADER_LENGTH_BYTES {
            return Err(PacketError::BufferTooSmall);
        }

        let id = HeaderIdentifier::try_from(buf[0])?;
        let mut start_idx = 1;
        let data_length = match id.encoding() {
            HeaderEncoding::Text | HeaderEncoding::Bytes => {
                if buf.len() < Self::MIN_UNICODE_OR_BYTE_SEQ_LENGTH_BYTES {
                    return Err(PacketError::BufferTooSmall);
                }
                // For Unicode Text and Byte Sequences, the payload length is encoded in the next
                // two bytes - this value includes 1 byte for the HI and 2 bytes for the length.
                let total_length = u16::from_be_bytes(
                    buf[Self::MIN_HEADER_LENGTH_BYTES..Self::MIN_UNICODE_OR_BYTE_SEQ_LENGTH_BYTES]
                        .try_into()
                        .expect("checked length"),
                ) as usize;
                let data_length = total_length
                    .checked_sub(Self::MIN_UNICODE_OR_BYTE_SEQ_LENGTH_BYTES)
                    .ok_or(PacketError::DataLength)?;
                start_idx = Self::MIN_UNICODE_OR_BYTE_SEQ_LENGTH_BYTES;
                data_length
            }
            HeaderEncoding::OneByte => 1, // Otherwise, a 1-byte payload is included.
            HeaderEncoding::FourBytes => 4, // Otherwise, a 4-byte payload is included.
        };
        trace!(?id, %data_length, "Parsed OBEX packet");

        if buf.len() < start_idx + data_length {
            return Err(PacketError::BufferTooSmall);
        }

        let data = &buf[start_idx..start_idx + data_length];
        let mut out_buf = vec![0; data_length];
        match id {
            HeaderIdentifier::Count => {
                Ok(Header::Count(u32::from_be_bytes(data[..].try_into().unwrap())))
            }
            HeaderIdentifier::Name => Ok(Header::Name(be_bytes_to_string(data)?)),
            HeaderIdentifier::Type => Ok(Header::Type(be_bytes_to_string(data)?)),
            HeaderIdentifier::Length => {
                Ok(Header::Length(u32::from_be_bytes(data[..].try_into().unwrap())))
            }
            HeaderIdentifier::TimeIso8601 => Ok(Header::TimeIso8601(be_bytes_to_string(data)?)),
            HeaderIdentifier::Time4Byte => {
                Ok(Header::Time4Byte(u32::from_be_bytes(data[..].try_into().unwrap())))
            }
            HeaderIdentifier::Description => Ok(Header::Description(be_bytes_to_string(data)?)),
            HeaderIdentifier::Target => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::Target(out_buf))
            }
            HeaderIdentifier::Http => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::Http(out_buf))
            }
            HeaderIdentifier::Body => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::Body(out_buf))
            }
            HeaderIdentifier::EndOfBody => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::EndOfBody(out_buf))
            }
            HeaderIdentifier::Who => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::Who(out_buf))
            }
            HeaderIdentifier::ConnectionId => {
                Ok(Header::ConnectionId(u32::from_be_bytes(data[..].try_into().unwrap())))
            }
            HeaderIdentifier::ApplicationParameters => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::ApplicationParameters(out_buf))
            }
            HeaderIdentifier::AuthenticationChallenge => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::AuthenticationChallenge(out_buf))
            }
            HeaderIdentifier::AuthenticationResponse => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::AuthenticationResponse(out_buf))
            }
            HeaderIdentifier::CreatorId => {
                Ok(Header::CreatorId(u32::from_be_bytes(data[..].try_into().unwrap())))
            }
            HeaderIdentifier::WanUuid => {
                let bytes: [u8; 16] =
                    data[..].try_into().map_err(|_| PacketError::BufferTooSmall)?;
                Ok(Header::WanUuid(Uuid::from_bytes(bytes)))
            }
            HeaderIdentifier::ObjectClass => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::ObjectClass(out_buf))
            }
            HeaderIdentifier::SessionParameters => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::SessionParameters(out_buf))
            }
            HeaderIdentifier::SessionSequenceNumber => {
                Ok(Header::SessionSequenceNumber(buf[start_idx]))
            }
            HeaderIdentifier::ActionId => Ok(Header::ActionId(buf[start_idx])),
            HeaderIdentifier::DestName => Ok(Header::DestName(be_bytes_to_string(data)?)),
            HeaderIdentifier::Permissions => {
                Ok(Header::Permissions(u32::from_be_bytes(data[..].try_into().unwrap())))
            }
            HeaderIdentifier::SingleResponseMode => Ok(Header::SingleResponseMode(buf[start_idx])),
            HeaderIdentifier::SingleResponseModeParameters => {
                Ok(Header::SingleResponseModeParameters(buf[start_idx]))
            }
            HeaderIdentifier::User(identifier) => {
                out_buf.copy_from_slice(&data[..]);
                Ok(Header::User(UserDefinedHeader { identifier, value: out_buf }))
            }
        }
    }
}

/// Attempts to convert the `buf` (big-endian) to a valid UTF-16 String.
fn be_bytes_to_string(buf: &[u8]) -> Result<String, PacketError> {
    if buf.len() == 0 {
        return Ok(String::new());
    }

    let unicode_array = buf
        .chunks_exact(2)
        .into_iter()
        .map(|a| u16::from_be_bytes([a[0], a[1]]))
        .collect::<Vec<u16>>();
    let mut text = String::from_utf16(&unicode_array).map_err(PacketError::external)?;
    // OBEX Strings are represented as null terminated Unicode text. See OBEX 1.5 Section 2.1.
    if !text.ends_with('\0') {
        return Err(PacketError::data("text missing null terminator"));
    }

    let _ = text.pop();
    Ok(text)
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

    #[fuchsia::test]
    fn header_encoding_from_identifier() {
        assert_eq!(HeaderIdentifier::SessionSequenceNumber.encoding(), HeaderEncoding::OneByte);
        assert_eq!(HeaderIdentifier::Count.encoding(), HeaderEncoding::FourBytes);
        assert_eq!(HeaderIdentifier::Name.encoding(), HeaderEncoding::Text);
        assert_eq!(HeaderIdentifier::Target.encoding(), HeaderEncoding::Bytes);
    }

    #[fuchsia::test]
    fn bytes_to_string_is_ok() {
        // Empty buf
        let converted = be_bytes_to_string(&[]).expect("can convert to String");
        assert_eq!(converted, "".to_string());

        // Just a null terminator.
        let buf = [0x00, 0x00];
        let converted = be_bytes_to_string(&buf).expect("can convert to String");
        assert_eq!(converted, "".to_string());

        // "hello"
        let buf = [0x00, 0x68, 0x00, 0x65, 0x00, 0x6c, 0x00, 0x6c, 0x00, 0x6f, 0x00, 0x00];
        let converted = be_bytes_to_string(&buf).expect("can convert to String");
        assert_eq!(converted, "hello".to_string());

        // "bob" with two null terminators - only last is removed.
        let buf = [0x00, 0x62, 0x00, 0x6f, 0x00, 0x62, 0x00, 0x00, 0x00, 0x00];
        let converted = be_bytes_to_string(&buf).expect("can convert to String");
        assert_eq!(converted, "bob\0".to_string());
    }

    #[fuchsia::test]
    fn bytes_to_string_missing_terminator_is_error() {
        // `foo.txt` with no null terminator.
        let buf =
            [0x00, 0x66, 0x00, 0x6f, 0x00, 0x6f, 0x00, 0x2e, 0x00, 0x74, 0x00, 0x78, 0x00, 0x74];
        let converted = be_bytes_to_string(&buf);
        assert_matches!(converted, Err(PacketError::Data(_)));
    }

    #[fuchsia::test]
    fn invalid_utf16_bytes_to_string_is_error() {
        // Invalid utf-16 bytes.
        let buf =
            [0xd8, 0x34, 0xdd, 0x1e, 0x00, 0x6d, 0x00, 0x75, 0xd8, 0x00, 0x00, 0x69, 0x00, 0x63];
        let converted = be_bytes_to_string(&buf);
        assert_matches!(converted, Err(PacketError::Other(_)));
    }

    #[fuchsia::test]
    fn decode_empty_header_is_error() {
        assert_matches!(Header::decode(&[]), Err(PacketError::BufferTooSmall));
    }

    #[fuchsia::test]
    fn decode_header_no_payload_is_error() {
        // Valid Count Header but no contents.
        assert_matches!(Header::decode(&[0xc0]), Err(PacketError::BufferTooSmall));
    }

    #[fuchsia::test]
    fn decode_byte_seq_invalid_length_is_error() {
        // Valid `Name` Header (Text) but the provided length is only 1 byte.
        let buf = [0x01, 0x07];
        assert_matches!(Header::decode(&buf), Err(PacketError::BufferTooSmall));

        // Valid `Target` Header (Byte Seq) but the provided length is only 1 byte.
        let buf = [0x46, 0x05];
        assert_matches!(Header::decode(&buf), Err(PacketError::BufferTooSmall));

        // Valid `Body` Header (Byte Seq) but the provided length is too small - it must be >= 3.
        let buf = [0x48, 0x00, 0x02];
        assert_matches!(Header::decode(&buf), Err(PacketError::DataLength));
    }

    #[fuchsia::test]
    fn decode_header_invalid_payload_is_error() {
        // The provided payload doesn't match the expected data length.
        let buf = [
            0x42, // `Type` Header (Text)
            0x00, 0x0b, // Total length = 10 implies a data length of 7.
            0x00, 0x68, 0x00, 0x69, 0x00,
            0x00, // Payload = "hi" with a null terminator -> 6 length
        ];
        assert_matches!(Header::decode(&buf), Err(PacketError::BufferTooSmall));

        // The provided payload doesn't match the expected data length.
        let buf = [
            0xc3, // `Length` Header (4 bytes)
            0x00, 0x00, 0x00, // Payload = 3 bytes (should be 4).
        ];
        assert_matches!(Header::decode(&buf), Err(PacketError::BufferTooSmall));

        // The provided payload doesn't match the expected data length.
        let buf = [
            0x94, // `ActionId` Header (Expect 1 byte payload)
        ];
        assert_matches!(Header::decode(&buf), Err(PacketError::BufferTooSmall));

        // The provided payload doesn't match the expected data length.
        let buf = [
            0x42, // `EndOfBody` Header (Byte seq)
            0x00, 0x06, // Total length = 6 implies a data length of 3.
            0x12, 0x34, // Payload = 2 bytes (should be 3),
        ];
        assert_matches!(Header::decode(&buf), Err(PacketError::BufferTooSmall));
    }

    #[fuchsia::test]
    fn decode_valid_header_success() {
        // Text Header
        let name_buf = [
            0x01, // HI = Name
            0x00, 0x17, // Total length = 23 bytes
            0x00, 0x54, 0x00, 0x48, 0x00, 0x49,
            0x00, // 20 byte payload = "THING.DOC" (utf-16)
            0x4e, 0x00, 0x47, 0x00, 0x2e, 0x00, 0x44, 0x00, 0x4f, 0x00, 0x43, 0x00, 0x00,
        ];
        let result = Header::decode(&name_buf).expect("can decode name header");
        assert_eq!(result, Header::Name("THING.DOC".to_string()));

        // Byte Sequence Header
        let object_class_buf = [
            0x51, // HI = Object Class
            0x00, 0x0a, // Total length = 10 bytes
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, // 7 byte payload
        ];
        let result = Header::decode(&object_class_buf).expect("can decode object class header");
        assert_eq!(result, Header::ObjectClass(vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06]));

        // One-byte Header
        let session_seq_num_buf = [
            0x93, // HI = Session Sequence Number
            0x05, // 1 byte payload
        ];
        let result = Header::decode(&session_seq_num_buf).expect("can decode valid name header");
        assert_eq!(result, Header::SessionSequenceNumber(5));

        // Four-byte Header
        let connection_id_buf = [
            0xcb, // HI = Session Sequence Number
            0x00, 0x00, 0x12, 0x34, // 4 byte payload
        ];
        let result = Header::decode(&connection_id_buf).expect("can decode connection id header");
        assert_eq!(result, Header::ConnectionId(0x1234));
    }

    #[fuchsia::test]
    fn decode_user_data_header_success() {
        let user_buf = [
            0xb3, // HI = Random User defined
            0x05, // Upper 2 bits of HI indicate 1 byte payload
        ];
        let result = Header::decode(&user_buf).expect("can decode user header");
        assert_eq!(result, Header::User(UserDefinedHeader { identifier: 0xb3, value: vec![0x05] }));
    }
}
