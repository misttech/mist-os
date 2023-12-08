// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{Array, Counted, Parse, ParseError, Validate};

use std::{fmt::Debug, ops::Deref as _};
use zerocopy::{little_endian as le, AsBytes, ByteSlice, FromBytes, FromZeroes, Ref, Unaligned};

pub(crate) const SELINUX_MAGIC: u32 = 0xf97cff8c;

pub(crate) const POLICYDB_STRING_MAX_LENGTH: u32 = 32;
pub(crate) const POLICYDB_SIGNATURE: &[u8] = b"SE Linux";

pub(crate) const POLICYDB_VERSION_MIN: u32 = 30;
pub(crate) const POLICYDB_VERSION_MAX: u32 = 33;

pub(crate) const CONFIG_MLS_FLAG: u32 = 1;
pub(crate) const CONFIG_HANDLE_UNKNOWN_REJECT_FLAG: u32 = 1 << 1;
pub(crate) const CONFIG_HANDLE_UNKNOWN_ALLOW_FLAG: u32 = 1 << 2;
pub(crate) const CONFIG_HANDLE_UNKNOWN_MASK: u32 =
    CONFIG_HANDLE_UNKNOWN_REJECT_FLAG | CONFIG_HANDLE_UNKNOWN_ALLOW_FLAG;

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct Magic(le::U32);

impl Validate for Magic {
    type Error = ParseError;

    fn validate(&self) -> Result<(), Self::Error> {
        let found_magic = self.0.get();
        if found_magic != SELINUX_MAGIC {
            Err(ParseError::InvalidMagic { found_magic })
        } else {
            Ok(())
        }
    }
}

pub(crate) type Signature<B> = Array<B, Ref<B, SignatureMetadata>, Ref<B, [u8]>>;

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct SignatureMetadata(le::U32);

impl Validate for SignatureMetadata {
    type Error = ParseError;

    /// [`SignatureMetadata`] has no constraints.
    fn validate(&self) -> Result<(), Self::Error> {
        let found_length = self.0.get();
        if found_length > POLICYDB_STRING_MAX_LENGTH {
            Err(ParseError::InvalidSignatureLength { found_length })
        } else {
            Ok(())
        }
    }
}

impl Counted for SignatureMetadata {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for Signature<B> {
    type Error = ParseError;

    fn validate(&self) -> Result<(), Self::Error> {
        let found_signature = &*self.data;
        if found_signature != POLICYDB_SIGNATURE {
            Err(ParseError::InvalidSignature {
                found_signature: found_signature.iter().map(Clone::clone).collect(),
            })
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, FromZeroes, FromBytes, AsBytes, Unaligned)]
#[repr(C, packed)]
pub(crate) struct PolicyVersion(le::U32);

impl PolicyVersion {
    pub fn policy_version(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for PolicyVersion {
    type Error = ParseError;

    fn validate(&self) -> Result<(), Self::Error> {
        let found_policy_version = self.0.get();
        if found_policy_version < POLICYDB_VERSION_MIN
            || found_policy_version > POLICYDB_VERSION_MAX
        {
            Err(ParseError::InvalidPolicyVersion { found_policy_version })
        } else {
            Ok(())
        }
    }
}

/// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct Config<B: ByteSlice + Debug + PartialEq> {
    handle_unknown: HandleUnknown,
    config: Ref<B, le::U32>,
}

impl<B: ByteSlice + Debug + PartialEq> Config<B> {
    pub fn handle_unknown(&self) -> &HandleUnknown {
        &self.handle_unknown
    }
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for Config<B> {
    type Error = ParseError;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let num_bytes = bytes.len();
        let (config, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(bytes).ok_or(ParseError::MissingData {
                type_name: "Config",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let found_config = config.deref().get();
        if found_config & CONFIG_MLS_FLAG == 0 {
            return Err(ParseError::ConfigMissingMlsFlag { found_config });
        }
        let handle_unknown = try_handle_unknown_fom_config(found_config)?;

        Ok((Self { handle_unknown, config }, tail))
    }
}

#[derive(Debug, PartialEq)]
pub enum HandleUnknown {
    Deny,
    Reject,
    Allow,
}

fn try_handle_unknown_fom_config(config: u32) -> Result<HandleUnknown, ParseError> {
    match config & CONFIG_HANDLE_UNKNOWN_MASK {
        CONFIG_HANDLE_UNKNOWN_ALLOW_FLAG => Ok(HandleUnknown::Allow),
        CONFIG_HANDLE_UNKNOWN_REJECT_FLAG => Ok(HandleUnknown::Reject),
        0 => Ok(HandleUnknown::Deny),
        _ => Err(ParseError::InvalidHandleUnknownConfigurationBits {
            masked_bits: (config & CONFIG_HANDLE_UNKNOWN_MASK),
        }),
    }
}

#[derive(Debug, FromZeroes, FromBytes, AsBytes, Unaligned)]
#[repr(C, packed)]
pub(crate) struct Counts {
    symbols_count: le::U32,
    object_context_count: le::U32,
}

impl Validate for Counts {
    type Error = ParseError;

    /// [`Counts`] have no internal consistency requirements.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::{super::test::as_parse_error, *};

    #[test]
    fn no_magic() {
        let mut bytes = [SELINUX_MAGIC.to_le_bytes().as_slice()].concat();
        // One byte short of magic.
        bytes.pop();
        let bytes = bytes;
        match Ref::<_, Magic>::parse(bytes.as_slice()).err().map(as_parse_error) {
            Some(ParseError::MissingData { type_name, type_size, num_bytes }) => {
                assert_eq!(3, num_bytes);
                assert_eq!(4, type_size);
                assert!(type_name.contains("Magic"));
            }
            parse_err => {
                assert!(false, "Expected Some(MissingData...), but got {:?}", parse_err);
            }
        }
    }

    #[test]
    fn invalid_magic() {
        let mut bytes = [SELINUX_MAGIC.to_le_bytes().as_slice()].concat();
        // Invalid first byte of magic.
        bytes[0] = bytes[0] + 1;
        let bytes = bytes;
        let expected_invalid_magic =
            u32::from_le_bytes(bytes.clone().as_slice().try_into().unwrap());

        assert_eq!(
            Some(ParseError::InvalidMagic { found_magic: expected_invalid_magic }),
            Ref::<_, Magic>::parse(bytes.as_slice()).err().map(as_parse_error)
        );
    }

    #[test]
    fn invalid_signature_length() {
        let invalid_signature_length = POLICYDB_STRING_MAX_LENGTH + 1;
        let bytes = [invalid_signature_length.to_le_bytes().as_slice()].concat();
        assert_eq!(
            Some(ParseError::InvalidSignatureLength { found_length: invalid_signature_length }),
            Ref::<_, SignatureMetadata>::parse(bytes.as_slice()).err().map(as_parse_error)
        );
    }

    #[test]
    fn missing_signature() {
        let bytes = [(1 as u32).to_le_bytes().as_slice()].concat();
        match Signature::parse(bytes.as_slice()).err().map(as_parse_error) {
            Some(ParseError::MissingSliceData {
                type_name: "u8",
                type_size: 1,
                num_items: 1,
                num_bytes: 0,
            }) => {}
            parse_err => {
                assert!(false, "Expected Some(MissingSliceData...), but got {:?}", parse_err);
            }
        }
    }

    #[test]
    fn invalid_signature() {
        let mut invalid_signature: Vec<u8> = POLICYDB_SIGNATURE.iter().map(Clone::clone).collect();
        // 'S' -> 'T': "TE Linux".
        invalid_signature[0] = invalid_signature[0] + 1;
        let invalid_signature = invalid_signature;

        let bytes = [
            (invalid_signature.len() as u32).to_le_bytes().as_slice(),
            invalid_signature.as_slice(),
        ]
        .concat();
        assert_eq!(
            Some(ParseError::InvalidSignature { found_signature: invalid_signature }),
            Signature::parse(bytes.as_slice()).err().map(as_parse_error)
        );
    }

    #[test]
    fn invalid_policy_version() {
        let bytes = [(POLICYDB_VERSION_MIN - 1).to_le_bytes().as_slice()].concat();
        assert_eq!(
            Some(ParseError::InvalidPolicyVersion {
                found_policy_version: POLICYDB_VERSION_MIN - 1
            }),
            Ref::<_, PolicyVersion>::parse(bytes.as_slice()).err().map(as_parse_error)
        );

        let bytes = [(POLICYDB_VERSION_MAX + 1).to_le_bytes().as_slice()].concat();
        assert_eq!(
            Some(ParseError::InvalidPolicyVersion {
                found_policy_version: POLICYDB_VERSION_MAX + 1
            }),
            Ref::<_, PolicyVersion>::parse(bytes.as_slice()).err().map(as_parse_error)
        );
    }

    #[test]
    fn config_missing_mls_flag() {
        let bytes = [(!CONFIG_MLS_FLAG).to_le_bytes().as_slice()].concat();
        match Config::parse(bytes.as_slice()).err() {
            Some(ParseError::ConfigMissingMlsFlag { .. }) => {}
            parse_err => {
                assert!(false, "Expected Some(ConfigMissingMlsFlag...), but got {:?}", parse_err);
            }
        }
    }

    #[test]
    fn invalid_handle_unknown() {
        let bytes = [(CONFIG_MLS_FLAG
            | CONFIG_HANDLE_UNKNOWN_ALLOW_FLAG
            | CONFIG_HANDLE_UNKNOWN_REJECT_FLAG)
            .to_le_bytes()
            .as_slice()]
        .concat();
        assert_eq!(
            Some(ParseError::InvalidHandleUnknownConfigurationBits {
                masked_bits: CONFIG_HANDLE_UNKNOWN_ALLOW_FLAG | CONFIG_HANDLE_UNKNOWN_REJECT_FLAG
            }),
            Config::parse(bytes.as_slice()).err()
        );
    }
}
