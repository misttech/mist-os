// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::parser::PolicyCursor;
use super::{
    array_type, array_type_validate_deref_both, Array, Counted, Parse, PolicyValidationContext,
    Validate, ValidateArray,
};
use crate::policy::error::{ParseError, ValidateError};

use zerocopy::{little_endian as le, FromBytes, Immutable, KnownLayout, Unaligned};

pub(super) const SELINUX_MAGIC: u32 = 0xf97cff8c;

pub(super) const POLICYDB_STRING_MAX_LENGTH: u32 = 32;
pub(super) const POLICYDB_SIGNATURE: &[u8] = b"SE Linux";

pub(super) const POLICYDB_VERSION_MIN: u32 = 30;
pub(super) const POLICYDB_VERSION_MAX: u32 = 33;

pub(super) const CONFIG_MLS_FLAG: u32 = 1;
pub(super) const CONFIG_HANDLE_UNKNOWN_REJECT_FLAG: u32 = 1 << 1;
pub(super) const CONFIG_HANDLE_UNKNOWN_ALLOW_FLAG: u32 = 1 << 2;
pub(super) const CONFIG_HANDLE_UNKNOWN_MASK: u32 =
    CONFIG_HANDLE_UNKNOWN_REJECT_FLAG | CONFIG_HANDLE_UNKNOWN_ALLOW_FLAG;

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct Magic(le::U32);

impl Validate for Magic {
    type Error = ValidateError;

    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        let found_magic = self.0.get();
        if found_magic != SELINUX_MAGIC {
            Err(ValidateError::InvalidMagic { found_magic })
        } else {
            Ok(())
        }
    }
}

array_type!(Signature, SignatureMetadata, Vec<u8>);

array_type_validate_deref_both!(Signature);

impl ValidateArray<SignatureMetadata, u8> for Signature {
    type Error = ValidateError;

    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &SignatureMetadata,
        items: &[u8],
    ) -> Result<(), Self::Error> {
        if items != POLICYDB_SIGNATURE {
            Err(ValidateError::InvalidSignature { found_signature: items.to_owned() })
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct SignatureMetadata(le::U32);

impl Validate for SignatureMetadata {
    type Error = ValidateError;

    /// [`SignatureMetadata`] has no constraints.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
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

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct PolicyVersion(le::U32);

impl PolicyVersion {
    pub fn policy_version(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for PolicyVersion {
    type Error = ValidateError;

    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
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

#[derive(Debug)]
pub(super) struct Config {
    handle_unknown: HandleUnknown,

    #[allow(dead_code)]
    config: le::U32,
}

impl Config {
    pub fn handle_unknown(&self) -> HandleUnknown {
        self.handle_unknown
    }
}

impl Parse for Config {
    type Error = ParseError;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let num_bytes = bytes.len();
        let (config, tail) =
            PolicyCursor::parse::<le::U32>(bytes).ok_or(ParseError::MissingData {
                type_name: "Config",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let found_config = config.get();
        if found_config & CONFIG_MLS_FLAG == 0 {
            return Err(ParseError::ConfigMissingMlsFlag { found_config });
        }
        let handle_unknown = try_handle_unknown_fom_config(found_config)?;

        Ok((Self { handle_unknown, config }, tail))
    }
}

impl Validate for Config {
    type Error = anyhow::Error;

    /// All validation for [`Config`] is necessary to parse it correctly. No additional validation
    /// required.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct Counts {
    symbols_count: le::U32,
    object_context_count: le::U32,
}

impl Validate for Counts {
    type Error = anyhow::Error;

    /// [`Counts`] have no internal consistency requirements.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::policy::parser::PolicyCursor;
    use crate::policy::testing::{as_parse_error, as_validate_error};
    use std::sync::Arc;

    macro_rules! validate_test {
        ($parse_output:ident, $data:expr, $result:tt, $check_impl:block) => {{
            let data = Arc::new($data);
            let mut context = crate::policy::PolicyValidationContext { data: data.clone() };
            fn check_by_value(
                $result: Result<(), <$parse_output as crate::policy::Validate>::Error>,
            ) {
                $check_impl
            }

            let (by_value_parsed, _tail) = $parse_output::parse(PolicyCursor::new(data.clone()))
                .expect("successful parse for validate test");
            let by_value_result = by_value_parsed.validate(&mut context);
            check_by_value(by_value_result);
        }};
    }

    // TODO: Run this test over `validate()`.
    #[test]
    fn no_magic() {
        let mut bytes = [SELINUX_MAGIC.to_le_bytes().as_slice()].concat();
        // One byte short of magic.
        bytes.pop();
        let data = Arc::new(bytes);
        assert_eq!(None, PolicyCursor::parse::<Magic>(PolicyCursor::new(data)),);
    }

    #[test]
    fn invalid_magic() {
        let mut bytes = [SELINUX_MAGIC.to_le_bytes().as_slice()].concat();
        // Invalid first byte of magic.
        bytes[0] = bytes[0] + 1;
        let bytes = bytes;
        let expected_invalid_magic =
            u32::from_le_bytes(bytes.clone().as_slice().try_into().unwrap());

        let data = Arc::new(bytes);
        let mut context = crate::policy::PolicyValidationContext { data: data.clone() };
        let (magic, tail) =
            PolicyCursor::parse::<Magic>(PolicyCursor::new(data.clone())).expect("magic");
        assert_eq!(0, tail.len());
        assert_eq!(
            Err(ValidateError::InvalidMagic { found_magic: expected_invalid_magic }),
            magic.validate(&mut context)
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
        match Signature::parse(PolicyCursor::new(Arc::new(bytes))).err().map(as_parse_error) {
            Some(ParseError::MissingData { type_name: "u8", type_size: 1, num_bytes: 0 }) => {}
            parse_err => {
                assert!(false, "Expected Some(MissingData...), but got {:?}", parse_err);
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
        let data = Arc::new(bytes);
        let mut context = crate::policy::PolicyValidationContext { data: data.clone() };
        let (policy_version, tail) =
            PolicyCursor::parse::<PolicyVersion>(PolicyCursor::new(data.clone())).expect("magic");
        assert_eq!(0, tail.len());
        assert_eq!(
            Err(ValidateError::InvalidPolicyVersion {
                found_policy_version: POLICYDB_VERSION_MIN - 1
            }),
            policy_version.validate(&mut context)
        );

        let bytes = [(POLICYDB_VERSION_MAX + 1).to_le_bytes().as_slice()].concat();
        let data = Arc::new(bytes);
        let mut context = crate::policy::PolicyValidationContext { data: data.clone() };
        let (policy_version, tail) =
            PolicyCursor::parse::<PolicyVersion>(PolicyCursor::new(data.clone())).expect("magic");
        assert_eq!(0, tail.len());
        assert_eq!(
            Err(ValidateError::InvalidPolicyVersion {
                found_policy_version: POLICYDB_VERSION_MAX + 1
            }),
            policy_version.validate(&mut context)
        );
    }

    #[test]
    fn config_missing_mls_flag() {
        let bytes = [(!CONFIG_MLS_FLAG).to_le_bytes().as_slice()].concat();
        match Config::parse(PolicyCursor::new(Arc::new(bytes))).err() {
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
            Config::parse(PolicyCursor::new(Arc::new(bytes))).err()
        );
    }
}
