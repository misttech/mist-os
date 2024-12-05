// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common types and utilities between MLDv2 and IGMPv3.
//!
//! See [`crate::igmp`] and [`crate::icmp::mld`] for implementations.

use core::borrow::Borrow;
use core::fmt::Debug;
use core::time::Duration;

use net_types::ip::IpAddress;
use net_types::MulticastAddr;

/// Creates a bitmask of [n] bits, [n] must be <= 31.
/// E.g. for n = 12 yields 0xFFF.
const fn bitmask(n: u8) -> u32 {
    assert!((n as u32) < u32::BITS);
    (1 << n) - 1
}

/// Requested value doesn't fit the representation.
#[derive(Debug, Eq, PartialEq)]
pub struct OverflowError;

/// Exact conversion failed.
#[derive(Debug, Eq, PartialEq)]
pub enum ExactConversionError {
    /// Equivalent to [`OverflowError`].
    Overflow,
    /// An exact representation is not possible.
    NotExact,
}

impl From<OverflowError> for ExactConversionError {
    fn from(OverflowError: OverflowError) -> Self {
        Self::Overflow
    }
}

/// The trait converts a code to a floating point value: in a linear fashion up
/// to `SWITCHPOINT` and then using a floating point representation to allow the
/// conversion of larger values. In MLD and IGMP there are different codes that
/// follow this pattern, e.g. QQIC, ResponseDelay ([RFC 3376 section 4.1], [RFC
/// 3810 section 5.1]), which all convert a code with the following underlying
/// structure:
///
///       0    NUM_EXP_BITS       NUM_MANT_BITS
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |X|      exp      |          mant         |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///
/// This trait simplifies the implementation by providing methods to perform the
/// conversion.
///
/// [RFC 3376 section 4.1]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-4.1
/// [RFC 3810 section 5.1]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-5.1
pub(crate) trait LinExpConversion<C: Debug + PartialEq + Copy + Clone>:
    Into<C> + Copy + Clone + Sized
{
    // Specified by Implementors
    /// Number of bits used for the mantissa.
    const NUM_MANT_BITS: u8;
    /// Number of bits used for the exponent.
    const NUM_EXP_BITS: u8;
    /// Perform a lossy conversion from the `C` type.
    ///
    /// Not all values in `C` can be exactly represented using the code and they
    /// will be rounded to a code that represents a value close the provided
    /// one.
    fn lossy_try_from(value: C) -> Result<Self, OverflowError>;

    // Provided for Implementors.
    /// How much the exponent needs to be incremented when performing the
    /// exponential conversion.
    const EXP_INCR: u32 = 3;
    /// Bitmask for the mantissa.
    const MANT_BITMASK: u32 = bitmask(Self::NUM_MANT_BITS);
    /// Bitmask for the exponent.
    const EXP_BITMASK: u32 = bitmask(Self::NUM_EXP_BITS);
    /// First value for which we start the exponential conversion.
    const SWITCHPOINT: u32 = 0x1 << (Self::NUM_MANT_BITS + Self::NUM_EXP_BITS);
    /// Prefix for capturing the mantissa.
    const MANT_PREFIX: u32 = 0x1 << Self::NUM_MANT_BITS;
    /// Maximum value the code supports.
    const MAX_VALUE: u32 =
        (Self::MANT_BITMASK | Self::MANT_PREFIX) << (Self::EXP_INCR + Self::EXP_BITMASK);

    /// Converts the provided code to a value: in a linear way until
    /// [Self::SWITCHPOINT] and using a floating representation for larger
    /// values.
    fn to_expanded(code: u16) -> u32 {
        let code = code.into();
        if code < Self::SWITCHPOINT {
            code
        } else {
            let mant = code & Self::MANT_BITMASK;
            let exp = (code >> Self::NUM_MANT_BITS) & Self::EXP_BITMASK;
            (mant | Self::MANT_PREFIX) << (Self::EXP_INCR + exp)
        }
    }

    /// Performs a lossy conversion from `value`.
    ///
    /// The function will always succeed for values within the valid range.
    /// However, the code might not exactly represent the provided input. E.g. a
    /// value of `MAX_VALUE - 1` cannot be exactly represented with a
    /// corresponding code, due the exponential representation. However, the
    /// function will be able to provide a code representing a value close to
    /// the provided one.
    ///
    /// If stronger guarantees are needed consider using
    /// [`LinExpConversion::exact_try_from`].
    fn lossy_try_from_expanded(value: u32) -> Result<u16, OverflowError> {
        if value > Self::MAX_VALUE {
            Err(OverflowError)
        } else if value < Self::SWITCHPOINT {
            // Given that Value is < Self::SWITCHPOINT, unwrapping here is safe.
            let code = value.try_into().unwrap();
            Ok(code)
        } else {
            let msb = (u32::BITS - value.leading_zeros()) - 1;
            let exp = msb - u32::from(Self::NUM_MANT_BITS);
            let mant = (value >> exp) & Self::MANT_BITMASK;
            // Unwrap guaranteed by the structure of the built int:
            let code = (Self::SWITCHPOINT | ((exp - Self::EXP_INCR) << Self::NUM_MANT_BITS) | mant)
                .try_into()
                .unwrap();
            Ok(code)
        }
    }

    /// Attempts an exact conversion from `value`.
    ///
    /// The function will succeed only for values within the valid range that
    /// can be exactly represented by the produced code. E.g. a value of
    /// `FLOATING_POINT_MAX_VALUE - 1` cannot be exactly represented with a
    /// corresponding, code due the exponential representation. In this case,
    /// the function will return an error.
    ///
    /// If a lossy conversion can be tolerated consider using
    /// [`LinExpConversion::lossy_try_from_expanded`].
    ///
    /// If the conversion is attempt is lossy, returns `Ok(None)`.
    fn exact_try_from(value: C) -> Result<Self, ExactConversionError> {
        let res = Self::lossy_try_from(value)?;
        if value == res.into() {
            Ok(res)
        } else {
            Err(ExactConversionError::NotExact)
        }
    }
}

create_protocol_enum!(
    /// Group/Multicast Record Types as defined in [RFC 3376 section 4.2.12] and
    /// [RFC 3810 section 5.2.12].
    ///
    /// [RFC 3376 section 4.2.12]:
    ///     https://tools.ietf.org/html/rfc3376#section-4.2.12
    /// [RFC 3810 section 5.2.12]:
    ///     https://www.rfc-editor.org/rfc/rfc3810#section-5.2.12
    #[allow(missing_docs)]
    #[derive(PartialEq, Copy, Clone)]
    pub enum GroupRecordType: u8 {
        ModeIsInclude, 0x01, "Mode Is Include";
        ModeIsExclude, 0x02, "Mode Is Exclude";
        ChangeToIncludeMode, 0x03, "Change To Include Mode";
        ChangeToExcludeMode, 0x04, "Change To Exclude Mode";
        AllowNewSources, 0x05, "Allow New Sources";
        BlockOldSources, 0x06, "Block Old Sources";
    }
);

/// QQIC (Querier's Query Interval Code) used in IGMPv3/MLDv2 messages, defined
/// in [RFC 3376 section 4.1.7] and [RFC 3810 section 5.1.9].
///
/// [RFC 3376 section 4.1.7]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-4.1.7
/// [RFC 3810 section 5.1.9]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-5.1.9
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub struct QQIC(u8);

impl QQIC {
    /// Creates a new `QQIC` allowing lossy conversion from `value`.
    pub fn new_lossy(value: Duration) -> Result<Self, OverflowError> {
        Self::lossy_try_from(value)
    }

    /// Creates a new `QQIC` rejecting lossy conversion from `value`.
    pub fn new_exact(value: Duration) -> Result<Self, ExactConversionError> {
        Self::exact_try_from(value)
    }
}

impl LinExpConversion<Duration> for QQIC {
    const NUM_MANT_BITS: u8 = 4;
    const NUM_EXP_BITS: u8 = 3;

    fn lossy_try_from(value: Duration) -> Result<Self, OverflowError> {
        let secs: u32 = value.as_secs().try_into().map_err(|_| OverflowError)?;
        let code = Self::lossy_try_from_expanded(secs)?.try_into().map_err(|_| OverflowError)?;
        Ok(Self(code))
    }
}

impl From<QQIC> for Duration {
    fn from(code: QQIC) -> Self {
        let secs: u64 = QQIC::to_expanded(code.0.into()).into();
        Duration::from_secs(secs)
    }
}

impl From<QQIC> for u8 {
    fn from(QQIC(v): QQIC) -> Self {
        v
    }
}

impl From<u8> for QQIC {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

/// QRV (Querier's Robustness Variable) used in IGMPv3/MLDv2 messages, defined
/// in [RFC 3376 section 4.1.6] and [RFC 3810 section 5.1.8].
///
/// [RFC 3376 section 4.1.6]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-4.1.6
/// [RFC 3810 section 5.1.8]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-5.1.8
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub struct QRV(u8);

impl QRV {
    const QRV_MAX: u8 = 7;

    /// Returns the Querier's Robustness Variable.
    ///
    /// From [RFC 3376 section 4.1.6]: If the querier's [Robustness Variable]
    /// exceeds 7, the maximum value of the QRV field, the QRV is set to zero.
    ///
    /// From [RFC 3810 section 5.1.8]: If the Querier's [Robustness Variable]
    /// exceeds 7 (the maximum value of the QRV field), the QRV field is set to
    /// zero.
    ///
    /// [RFC 3376 section 4.1.6]:
    ///     https://datatracker.ietf.org/doc/html/rfc3376#section-4.1.6
    ///
    /// [RFC 3810 section 5.1.8]:
    ///     https://datatracker.ietf.org/doc/html/rfc3810#section-5.1.8
    pub fn new(robustness_value: u8) -> Self {
        if robustness_value > Self::QRV_MAX {
            return QRV(0);
        }
        QRV(robustness_value)
    }
}

impl From<QRV> for u8 {
    fn from(qrv: QRV) -> u8 {
        qrv.0
    }
}

/// A trait abstracting a multicast group record in MLDv2 or IGMPv3.
///
/// This trait facilitates the nested iterators required for implementing group
/// records (iterator of groups, each of which with an iterator of sources)
/// without propagating the inner iterator types far up.
///
/// An implementation for tuples of `(group, record_type, iterator)` is
/// provided.
pub trait GmpReportGroupRecord<A: IpAddress> {
    /// Returns the multicast group this report refers to.
    fn group(&self) -> MulticastAddr<A>;

    /// Returns record type to insert in the record entry.
    fn record_type(&self) -> GroupRecordType;

    /// Returns an iterator over the sources in the report.
    fn sources(&self) -> impl Iterator<Item: Borrow<A>> + '_;
}

impl<A, I> GmpReportGroupRecord<A> for (MulticastAddr<A>, GroupRecordType, I)
where
    A: IpAddress,
    I: Iterator<Item: Borrow<A>> + Clone,
{
    fn group(&self) -> MulticastAddr<A> {
        self.0
    }

    fn record_type(&self) -> GroupRecordType {
        self.1
    }

    fn sources(&self) -> impl Iterator<Item: Borrow<A>> + '_ {
        self.2.clone()
    }
}
