// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Traits and utilities to handle length-type-value structures.
pub mod ltv;

use crate::packet_encoding::{Encodable, Error as PacketError};

/// Bluetooth Device Address that uniquely identifies the device
/// to another Bluetooth device.
/// See Core spec v5.3 Vol 2, Part B section 1.2.
pub type Address = [u8; 6];

/// See Core spec v5.3 Vol 3, Part C section 15.1.1.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AddressType {
    Public = 0x00,
    Random = 0x01,
}

impl AddressType {
    pub const BYTE_SIZE: usize = 1;
}

impl TryFrom<u8> for AddressType {
    type Error = PacketError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Public),
            0x01 => Ok(Self::Random),
            _ => Err(PacketError::OutOfRange),
        }
    }
}

/// Advertising Set ID which is 1 byte long.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdvertisingSetId(pub u8);

impl AdvertisingSetId {
    // Byte size if this is to be encoded.
    pub const BYTE_SIZE: usize = 1;
}

/// SyncInfo Interval value which is 2 bytes long.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaInterval(pub u16);

impl PaInterval {
    pub const BYTE_SIZE: usize = 2;
    pub const UNKNOWN_VALUE: u16 = 0xFFFF;

    pub const fn unknown() -> Self {
        Self(Self::UNKNOWN_VALUE)
    }
}

impl Encodable for PaInterval {
    type Error = PacketError;

    /// Encodees the PaInterval to 2 byte value using little endian encoding.
    fn encode(&self, buf: &mut [u8]) -> core::result::Result<(), Self::Error> {
        if buf.len() < Self::BYTE_SIZE {
            return Err(PacketError::BufferTooSmall);
        }
        buf[0..Self::BYTE_SIZE].copy_from_slice(&self.0.to_le_bytes());
        Ok(())
    }

    fn encoded_len(&self) -> core::primitive::usize {
        Self::BYTE_SIZE
    }
}

/// Coding Format as defined by the Assigned Numbers Document. Section 2.11.
/// Referenced in the Core Spec 5.3, Volume 4, Part E, Section 7 as well as
/// various other profile specifications.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum CodingFormat {
    MuLawLog,
    ALawLog,
    Cvsd,
    Transparent,
    LinearPcm,
    Msbc,
    Lc3,
    G729a,
    VendorSpecific,
    Unrecognized(u8),
}

impl From<u8> for CodingFormat {
    fn from(value: u8) -> Self {
        match value {
            0x00 => Self::MuLawLog,
            0x01 => Self::ALawLog,
            0x02 => Self::Cvsd,
            0x03 => Self::Transparent,
            0x04 => Self::LinearPcm,
            0x05 => Self::Msbc,
            0x06 => Self::Lc3,
            0x07 => Self::G729a,
            0xFF => Self::VendorSpecific,
            x => Self::Unrecognized(x),
        }
    }
}

impl From<CodingFormat> for u8 {
    fn from(value: CodingFormat) -> Self {
        match value {
            CodingFormat::MuLawLog => 0x00,
            CodingFormat::ALawLog => 0x01,
            CodingFormat::Cvsd => 0x02,
            CodingFormat::Transparent => 0x03,
            CodingFormat::LinearPcm => 0x04,
            CodingFormat::Msbc => 0x05,
            CodingFormat::Lc3 => 0x06,
            CodingFormat::G729a => 0x07,
            CodingFormat::VendorSpecific => 0xFF,
            CodingFormat::Unrecognized(x) => x,
        }
    }
}

impl core::fmt::Display for CodingFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodingFormat::MuLawLog => write!(f, "µ-law log"),
            CodingFormat::ALawLog => write!(f, "A-law log"),
            CodingFormat::Cvsd => write!(f, "CVSD"),
            CodingFormat::Transparent => write!(f, "Transparent"),
            CodingFormat::LinearPcm => write!(f, "Linear PCM"),
            CodingFormat::Msbc => write!(f, "mSBC"),
            CodingFormat::Lc3 => write!(f, "LC3"),
            CodingFormat::G729a => write!(f, "G.729A"),
            CodingFormat::VendorSpecific => write!(f, "Vendor Specific"),
            CodingFormat::Unrecognized(x) => write!(f, "Unrecognized ({x})"),
        }
    }
}

/// Codec_ID communicated by a basic audio profile service/role.
#[derive(Debug, Clone, PartialEq)]
pub enum CodecId {
    /// From the Assigned Numbers. Format will not be
    /// `CodingFormat::VendorSpecific`
    Assigned(CodingFormat),
    VendorSpecific {
        company_id: crate::CompanyId,
        vendor_specific_codec_id: u16,
    },
}

impl CodecId {
    pub const BYTE_SIZE: usize = 5;
}

impl crate::packet_encoding::Decodable for CodecId {
    type Error = crate::packet_encoding::Error;

    fn decode(buf: &[u8]) -> core::result::Result<(Self, usize), Self::Error> {
        if buf.len() < 5 {
            return Err(crate::packet_encoding::Error::UnexpectedDataLength);
        }
        let format = buf[0].into();
        if format != CodingFormat::VendorSpecific {
            // Maybe don't ignore the company and vendor id, and check if they are wrong.
            return Ok((Self::Assigned(format), 5));
        }
        let company_id = u16::from_le_bytes([buf[1], buf[2]]).into();
        let vendor_specific_codec_id = u16::from_le_bytes([buf[3], buf[4]]);
        Ok((Self::VendorSpecific { company_id, vendor_specific_codec_id }, 5))
    }
}

#[cfg(test)]
mod tests {
    use crate::packet_encoding::Decodable;

    use super::*;

    #[test]
    fn encode_pa_interval() {
        let mut buf = [0; PaInterval::BYTE_SIZE];
        let interval = PaInterval(0x1004);

        interval.encode(&mut buf[..]).expect("should succeed");
        assert_eq!(buf, [0x04, 0x10]);
    }

    #[test]
    fn encode_pa_interval_fails() {
        let mut buf = [0; 1]; // Not enough buffer space.
        let interval = PaInterval(0x1004);

        interval.encode(&mut buf[..]).expect_err("should fail");
    }

    #[test]
    fn decode_codec_id() {
        let assigned = [0x01, 0x00, 0x00, 0x00, 0x00];
        let (codec_id, _) = CodecId::decode(&assigned[..]).expect("should succeed");
        assert_eq!(codec_id, CodecId::Assigned(CodingFormat::ALawLog));

        let vendor_specific = [0xFF, 0x36, 0xFD, 0x11, 0x22];
        let (codec_id, _) = CodecId::decode(&vendor_specific[..]).expect("should succeed");
        assert_eq!(
            codec_id,
            CodecId::VendorSpecific {
                company_id: (0xFD36 as u16).into(),
                vendor_specific_codec_id: 0x2211
            }
        );
    }
}
