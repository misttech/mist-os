// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    array_type, array_type_validate_deref_both, error::ValidateError, parser::ParseStrategy, Array,
    Counted, Parse, ParseError, Validate, ValidateArray,
};

use std::fmt::Debug;
use zerocopy::{little_endian as le, FromBytes, FromZeroes, NoCell, Unaligned};

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

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct Magic(le::U32);

impl Validate for Magic {
    type Error = ValidateError;

    fn validate(&self) -> Result<(), Self::Error> {
        let found_magic = self.0.get();
        if found_magic != SELINUX_MAGIC {
            Err(ValidateError::InvalidMagic { found_magic })
        } else {
            Ok(())
        }
    }
}

array_type!(Signature, PS, PS::Output<SignatureMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(Signature);

impl<PS: ParseStrategy> ValidateArray<SignatureMetadata, u8> for Signature<PS> {
    type Error = ValidateError;

    fn validate_array<'a>(
        _metadata: &'a SignatureMetadata,
        data: &'a [u8],
    ) -> Result<(), Self::Error> {
        if data != POLICYDB_SIGNATURE {
            Err(ValidateError::InvalidSignature { found_signature: data.to_owned() })
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct SignatureMetadata(le::U32);

impl Validate for SignatureMetadata {
    type Error = ValidateError;

    /// [`SignatureMetadata`] has no constraints.
    fn validate(&self) -> Result<(), Self::Error> {
        let found_length = self.0.get();
        if found_length > POLICYDB_STRING_MAX_LENGTH {
            Err(ValidateError::InvalidSignatureLength { found_length })
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

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct PolicyVersion(le::U32);

impl PolicyVersion {
    pub fn policy_version(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for PolicyVersion {
    type Error = ValidateError;

    fn validate(&self) -> Result<(), Self::Error> {
        let found_policy_version = self.0.get();
        if found_policy_version < POLICYDB_VERSION_MIN
            || found_policy_version > POLICYDB_VERSION_MAX
        {
            Err(ValidateError::InvalidPolicyVersion { found_policy_version })
        } else {
            Ok(())
        }
    }
}

/// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct Config<PS: ParseStrategy> {
    handle_unknown: HandleUnknown,
    config: PS::Output<le::U32>,
}

impl<PS: ParseStrategy> Config<PS> {
    pub fn handle_unknown(&self) -> &HandleUnknown {
        &self.handle_unknown
    }
}

impl<PS: ParseStrategy> Parse<PS> for Config<PS> {
    type Error = ParseError;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let num_bytes = bytes.len();
        let (config, tail) = PS::parse::<le::U32>(bytes).ok_or(ParseError::MissingData {
            type_name: "Config",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let found_config = PS::deref(&config).get();
        if found_config & CONFIG_MLS_FLAG == 0 {
            return Err(ParseError::ConfigMissingMlsFlag { found_config });
        }
        let handle_unknown = try_handle_unknown_fom_config(found_config)?;

        Ok((Self { handle_unknown, config }, tail))
    }
}

impl<PS: ParseStrategy> Validate for Config<PS> {
    type Error = anyhow::Error;

    /// All validation for [`Config`] is necessary to parse it correctly. No additional validation
    /// required.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
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

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct Counts {
    symbols_count: le::U32,
    object_context_count: le::U32,
}

impl Validate for Counts {
    type Error = anyhow::Error;

    /// [`Counts`] have no internal consistency requirements.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::test::as_validate_error;

    use super::{
        super::{
            parser::{ByRef, ByValue},
            test::{as_parse_error, validate_test},
        },
        *,
    };

    use std::io::Cursor;

    // TODO: Run this test over `validate()`.
    #[test]
    fn no_magic() {
        let mut bytes = [SELINUX_MAGIC.to_le_bytes().as_slice()].concat();
        // One byte short of magic.
        bytes.pop();
        let bytes = bytes;
        assert_eq!(None, ByRef::parse::<Magic>(ByRef::new(bytes.as_slice())),);
        assert_eq!(None, ByValue::parse::<Magic>(ByValue::new(Cursor::new(bytes))),);
    }

    #[test]
    fn invalid_magic() {
        let mut bytes = [SELINUX_MAGIC.to_le_bytes().as_slice()].concat();
        // Invalid first byte of magic.
        bytes[0] = bytes[0] + 1;
        let bytes = bytes;
        let expected_invalid_magic =
            u32::from_le_bytes(bytes.clone().as_slice().try_into().unwrap());

        let (magic, tail) = ByRef::parse::<Magic>(ByRef::new(bytes.as_slice())).expect("magic");
        assert_eq!(0, tail.len());
        assert_eq!(
            Err(ValidateError::InvalidMagic { found_magic: expected_invalid_magic }),
            magic.validate()
        );

        let (magic, tail) =
            ByValue::parse::<Magic>(ByValue::new(Cursor::new(bytes))).expect("magic");
        assert_eq!(0, tail.len());
        assert_eq!(
            Err(ValidateError::InvalidMagic { found_magic: expected_invalid_magic }),
            magic.validate()
        );
    }

    #[test]
    fn invalid_signature_length() {
        const INVALID_SIGNATURE_LENGTH: u32 = POLICYDB_STRING_MAX_LENGTH + 1;
        let bytes: Vec<u8> = [
            INVALID_SIGNATURE_LENGTH.to_le_bytes().as_slice(),
            [42u8; INVALID_SIGNATURE_LENGTH as usize].as_slice(),
        ]
        .concat();

        validate_test!(Signature, bytes, result, {
            assert_eq!(
                Some(ValidateError::InvalidSignatureLength {
                    found_length: INVALID_SIGNATURE_LENGTH
                }),
                result.err().map(as_validate_error),
            );
        });
    }

    #[test]
    fn missing_signature() {
        let bytes = [(1 as u32).to_le_bytes().as_slice()].concat();
        match Signature::parse(ByRef::new(bytes.as_slice())).err().map(as_parse_error) {
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
        // Invalid signature "TE Linux" is not "SE Linux".
        const INVALID_SIGNATURE: &[u8] = b"TE Linux";

        let bytes =
            [(INVALID_SIGNATURE.len() as u32).to_le_bytes().as_slice(), INVALID_SIGNATURE].concat();

        validate_test!(Signature, bytes, result, {
            assert_eq!(
                Some(ValidateError::InvalidSignature {
                    found_signature: INVALID_SIGNATURE.to_owned()
                }),
                result.err().map(as_validate_error),
            );
        });
    }

    #[test]
    fn invalid_policy_version() {
        let bytes = [(POLICYDB_VERSION_MIN - 1).to_le_bytes().as_slice()].concat();
        let (policy_version, tail) =
            ByRef::parse::<PolicyVersion>(ByRef::new(bytes.as_slice())).expect("magic");
        assert_eq!(0, tail.len());
        assert_eq!(
            Err(ValidateError::InvalidPolicyVersion {
                found_policy_version: POLICYDB_VERSION_MIN - 1
            }),
            policy_version.validate()
        );

        let (policy_version, tail) =
            ByValue::parse::<PolicyVersion>(ByValue::new(Cursor::new(bytes))).expect("magic");
        assert_eq!(0, tail.len());
        assert_eq!(
            Err(ValidateError::InvalidPolicyVersion {
                found_policy_version: POLICYDB_VERSION_MIN - 1
            }),
            policy_version.validate()
        );

        let bytes = [(POLICYDB_VERSION_MAX + 1).to_le_bytes().as_slice()].concat();
        let (policy_version, tail) =
            ByRef::parse::<PolicyVersion>(ByRef::new(bytes.as_slice())).expect("magic");
        assert_eq!(0, tail.len());
        assert_eq!(
            Err(ValidateError::InvalidPolicyVersion {
                found_policy_version: POLICYDB_VERSION_MAX + 1
            }),
            policy_version.validate()
        );

        let (policy_version, tail) =
            ByValue::parse::<PolicyVersion>(ByValue::new(Cursor::new(bytes))).expect("magic");
        assert_eq!(0, tail.len());
        assert_eq!(
            Err(ValidateError::InvalidPolicyVersion {
                found_policy_version: POLICYDB_VERSION_MAX + 1
            }),
            policy_version.validate()
        );
    }

    #[test]
    fn config_missing_mls_flag() {
        let bytes = [(!CONFIG_MLS_FLAG).to_le_bytes().as_slice()].concat();
        match Config::parse(ByRef::new(bytes.as_slice())).err() {
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
            Config::parse(ByRef::new(bytes.as_slice())).err()
        );
    }
}
