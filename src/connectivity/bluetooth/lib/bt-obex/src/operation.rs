// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use packet_encoding::decodable_enum;

use crate::error::PacketError;

#[derive(Debug, Clone, PartialEq)]
#[repr(u8)]
pub enum OpCode {
    Connect = 0x80,
    Disconnect = 0x81,
    Put = 0x02,
    PutFinal = 0x82,
    Get = 0x03,
    GetFinal = 0x83,
    Reserved = 0x04,
    ReservedFinal = 0x84,
    SetPath = 0x85,
    Action = 0x06,
    ActionFinal = 0x86,
    Session = 0x87,
    /// 0x08 to 0x0F are reserved and not used in OBEX.
    /// 0x10 to 0x1f are user defined.
    User(u8),
    Abort = 0xff,
}

impl OpCode {
    fn final_bit_set(v: u8) -> bool {
        (v & 0x80) != 0
    }

    fn is_user(code: u8) -> bool {
        // Defined in OBEX 1.5 Section 3.4.
        code >= 0x10 && code <= 0x1f
    }

    fn is_reserved(code: u8) -> bool {
        // Defined in OBEX 1.5 Section 3.4.
        code >= 0x08 && code <= 0x0f
    }

    /// Returns true if the Final bit is set.
    pub fn is_final(&self) -> bool {
        let opcode_raw: u8 = self.into();
        Self::final_bit_set(opcode_raw)
    }
}

impl Into<u8> for &OpCode {
    fn into(self) -> u8 {
        match &self {
            OpCode::Connect => 0x80,
            OpCode::Disconnect => 0x81,
            OpCode::Put => 0x02,
            OpCode::PutFinal => 0x82,
            OpCode::Get => 0x03,
            OpCode::GetFinal => 0x83,
            OpCode::Reserved => 0x04,
            OpCode::ReservedFinal => 0x84,
            OpCode::SetPath => 0x85,
            OpCode::Action => 0x06,
            OpCode::ActionFinal => 0x86,
            OpCode::Session => 0x87,
            OpCode::User(v) => *v,
            OpCode::Abort => 0xff,
        }
    }
}

impl TryFrom<u8> for OpCode {
    type Error = PacketError;

    fn try_from(src: u8) -> Result<OpCode, Self::Error> {
        // The Abort operation is unique in that it uses all bits in the opcode.
        if src == 0xff {
            return Ok(OpCode::Abort);
        }

        // Per OBEX 1.5 Section 3.4, only bits 0-4 are used to determine the OpCode. Bits 5,6
        // should be unset and are ignored. Bit 7 (msb) represents the final bit.
        const FINAL_BIT_AND_OPCODE_BITMASK: u8 = 0x9f;
        const OPCODE_BITMASK: u8 = 0x1f;
        let src = src & FINAL_BIT_AND_OPCODE_BITMASK;
        let is_final = OpCode::final_bit_set(src);
        // Check the lower 5 bits for opcode.
        match src & OPCODE_BITMASK {
            0x00 if is_final => Ok(OpCode::Connect),
            0x01 if is_final => Ok(OpCode::Disconnect),
            0x02 if is_final => Ok(OpCode::PutFinal),
            0x02 => Ok(OpCode::Put),
            0x03 if is_final => Ok(OpCode::GetFinal),
            0x03 => Ok(OpCode::Get),
            0x04 if is_final => Ok(OpCode::ReservedFinal),
            0x04 => Ok(OpCode::Reserved),
            0x05 if is_final => Ok(OpCode::SetPath),
            0x06 if is_final => Ok(OpCode::ActionFinal),
            0x06 => Ok(OpCode::Action),
            0x07 if is_final => Ok(OpCode::Session),
            v if OpCode::is_user(v) => Ok(OpCode::User(src)), // Save the final bit.
            v if OpCode::is_reserved(v) => Err(PacketError::Reserved),
            _ => Err(PacketError::OpCode(src)),
        }
    }
}

decodable_enum! {
    /// Response codes that an OBEX server may send to the Client after receiving a request.
    /// The most significant bit of the response code is the Final Bit. This is always set in OBEX
    /// response codes - see OBEX 1.5 Section 3.2.
    /// Defined in OBEX 1.5 Section 3.2.1.
    enum ResponseCode<u8, PacketError, HeaderEncoding> {
        Continue = 0x90,
        Ok = 0xa0,
        Created = 0xa1,
        Accepted = 0xa2,
        NonAuthoritativeInformation = 0xa3,
        NoContent = 0xa4,
        ResetContent = 0xa5,
        PartialContent = 0xa6,
        MultipleChoices = 0xb0,
        MovedPermanently = 0xb1,
        MovedTemporarily = 0xb2,
        SeeOther = 0xb3,
        NotModified = 0xb4,
        UseProxy = 0xb5,
        BadRequest = 0xc0,
        Unauthorized = 0xc1,
        PaymentRequired = 0xc2,
        Forbidden = 0xc3,
        NotFound = 0xc4,
        MethodNotAllowed = 0xc5,
        NotAcceptable = 0xc6,
        ProxyAuthenticationRequired = 0xc7,
        RequestTimeOut = 0xc8,
        Conflict = 0xc9,
        Gone = 0xca,
        LengthRequired = 0xcb,
        PreconditionFailed = 0xcc,
        RequestedEntityTooLarge = 0xcd,
        RequestedUrlTooLarge = 0xce,
        UnsupportedMediaType = 0xcf,
        InternalServerError = 0xd0,
        NotImplemented = 0xd1,
        BadGateway = 0xd2,
        ServiceUnavailable = 0xd3,
        GatewayTimeout = 0xd4,
        HttpVersionNotSupported = 0xd5,
        DatabaseFull = 0xe0,
        DatabaseLocked = 0xe1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;

    #[fuchsia::test]
    fn convert_opcode_success() {
        // Roundtrip with final disabled should succeed.
        let raw = 0x02;
        let converted = OpCode::try_from(raw).expect("valid opcode");
        assert_eq!(converted, OpCode::Put);
        assert!(!converted.is_final());
        let converted_raw: u8 = (&converted).into();
        assert_eq!(converted_raw, raw);

        // Roundtrip with final enabled should succeed.
        let raw = 0x84;
        let converted = OpCode::try_from(raw).expect("valid opcode");
        assert_eq!(converted, OpCode::ReservedFinal);
        assert!(converted.is_final());
        let converted_raw: u8 = (&converted).into();
        assert_eq!(converted_raw, raw);

        // Roundtrip for Abort should succeed (special).
        let raw = 0xff;
        let converted = OpCode::try_from(raw).expect("valid opcode");
        assert_eq!(converted, OpCode::Abort);
        assert!(converted.is_final());
        let converted_raw: u8 = (&converted).into();
        assert_eq!(converted_raw, raw);

        // Roundtrip for an opcode with bits 5,6 set is OK. The bits are unused, and the
        // receiving side should ignore.
        let raw = 0xe5; // SetPath (0x85) with bits 5,6 set.
        let converted = OpCode::try_from(raw).expect("valid opcode");
        assert_eq!(converted, OpCode::SetPath);
        assert!(converted.is_final());
        let converted_raw: u8 = (&converted).into();
        assert_eq!(converted_raw, 0x85); // We will never set bits 5,6.
    }

    #[fuchsia::test]
    fn convert_user_opcode_success() {
        // User opcode with final bit unset.
        let user = 0x1a;
        let converted = OpCode::try_from(user).expect("valid opcode");
        assert_eq!(converted, OpCode::User(0x1a));
        assert!(!converted.is_final());
        let converted_raw: u8 = (&converted).into();
        assert_eq!(converted_raw, user);

        // User opcode with final bit set.
        let user = 0x9d;
        let converted = OpCode::try_from(user).expect("valid opcode");
        assert_eq!(converted, OpCode::User(0x9d));
        assert!(converted.is_final());
        let converted_raw: u8 = (&converted).into();
        // Final bit should be preserved when converting back.
        assert_eq!(converted_raw, user);

        // User opcode with bits 5,6 set. Bits 5,6 should be ignored.
        let user = 0xf3;
        let converted = OpCode::try_from(user).expect("valid opcode");
        assert_eq!(converted, OpCode::User(0x93)); // Bits 5,6 should be zeroed out.
        assert!(converted.is_final());
        let converted_raw: u8 = (&converted).into();
        assert_eq!(converted_raw, 0x93);
    }

    #[fuchsia::test]
    fn convert_invalid_opcode_is_error() {
        // A Disconnect OpCode without the final bit set is invalid.
        let invalid = 0x01;
        assert_matches!(OpCode::try_from(invalid), Err(PacketError::OpCode(_)));
        // Opcode is reserved for future use.
        let reserved = 0x08;
        assert_matches!(OpCode::try_from(reserved), Err(PacketError::Reserved));
        // Opcode is reserved for future use (final bit set).
        let reserved = 0x8f;
        assert_matches!(OpCode::try_from(reserved), Err(PacketError::Reserved));
    }
}
