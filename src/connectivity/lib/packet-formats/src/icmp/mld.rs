// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Multicast Listener Discovery Protocol.
//!
//! Wire serialization and deserialization functions.

use core::fmt::Debug;
use core::mem::size_of;
use core::ops::Deref;
use core::time::Duration;

use net_types::ip::{Ipv6, Ipv6Addr};
use net_types::MulticastAddr;
use packet::{
    records::{ParsedRecord, RecordParseResult, Records, RecordsImpl, RecordsImplLayout},
    serialize::InnerPacketBuilder,
    BufferView,
};
use zerocopy::{
    byteorder::network_endian::U16, AsBytes, ByteSlice, FromBytes, FromZeros, NoCell, Ref,
    Unaligned,
};

use crate::error::{ParseError, ParseResult, UnrecognizedProtocolCode};
use crate::icmp::{IcmpIpExt, IcmpMessage, IcmpPacket, IcmpUnusedCode, MessageBody};

/// An ICMPv6 packet with an MLD message.
#[allow(missing_docs)]
#[derive(Debug)]
pub enum MldPacket<B: ByteSlice> {
    MulticastListenerQuery(IcmpPacket<Ipv6, B, MulticastListenerQuery>),
    MulticastListenerReport(IcmpPacket<Ipv6, B, MulticastListenerReport>),
    MulticastListenerDone(IcmpPacket<Ipv6, B, MulticastListenerDone>),
    MulticastListenerReportV2(IcmpPacket<Ipv6, B, MulticastListenerReportV2>),
}

create_protocol_enum!(
    /// Multicast Record Types as defined in [RFC 3810 section 5.2.12]
    ///
    /// [RFC 3810 section 5.2.12]: https://www.rfc-editor.org/rfc/rfc3810#section-5.2.12
    #[allow(missing_docs)]
    #[derive(PartialEq, Copy, Clone)]
    pub enum Mldv2MulticastRecordType: u8 {
        ModeIsInclude, 0x01, "Mode Is Include";
        ModeIsExclude, 0x02, "Mode Is Exclude";
        ChangeToIncludeMode, 0x03, "Change To Include Mode";
        ChangeToExcludeMode, 0x04, "Change To Exclude Mode";
        AllowNewSources, 0x05, "Allow New Sources";
        BlockOldSources, 0x06, "Block Old Sources";
    }
);

/// Fixed information for an MLDv2 Report's Multicast Record, per
/// [RFC 3810 section 5.2].
///
/// [RFC 3810 section 5.2]: https://www.rfc-editor.org/rfc/rfc3810#section-5.2
#[derive(Copy, Clone, Debug, AsBytes, FromZeros, FromBytes, NoCell, Unaligned)]
#[repr(C)]
pub struct MulticastRecordHeader {
    record_type: u8,
    aux_data_len: u8,
    number_of_sources: U16,
    multicast_address: Ipv6Addr,
}

impl MulticastRecordHeader {
    /// Returns the number of sources.
    pub fn number_of_sources(&self) -> u16 {
        self.number_of_sources.get()
    }

    /// Returns the type of the record.
    pub fn record_type(&self) -> Result<Mldv2MulticastRecordType, UnrecognizedProtocolCode<u8>> {
        Mldv2MulticastRecordType::try_from(self.record_type)
    }

    /// Returns the multicast address.
    pub fn multicast_addr(&self) -> &Ipv6Addr {
        &self.multicast_address
    }
}

/// Wire representation of an MLDv2 Report's Multicast Record, per
/// [RFC 3810 section 5.2].
///
/// [RFC 3810 section 5.2]: https://www.rfc-editor.org/rfc/rfc3810#section-5.2
pub struct MulticastRecord<B> {
    header: Ref<B, MulticastRecordHeader>,
    sources: Ref<B, [Ipv6Addr]>,
}

impl<B: ByteSlice> MulticastRecord<B> {
    /// Returns the multicast record header.
    pub fn header(&self) -> &MulticastRecordHeader {
        self.header.deref()
    }

    /// Returns the multicast record's sources.
    pub fn sources(&self) -> &[Ipv6Addr] {
        self.sources.deref()
    }
}

/// An implementation of MLDv2 report's records parsing.
#[derive(Copy, Clone, Debug)]
pub enum Mldv2ReportRecords {}

impl RecordsImplLayout for Mldv2ReportRecords {
    type Context = usize;
    type Error = ParseError;
}

impl<'a> RecordsImpl<'a> for Mldv2ReportRecords {
    type Record = MulticastRecord<&'a [u8]>;

    fn parse_with_context<BV: BufferView<&'a [u8]>>(
        data: &mut BV,
        _ctx: &mut usize,
    ) -> RecordParseResult<MulticastRecord<&'a [u8]>, ParseError> {
        let header = data
            .take_obj_front::<MulticastRecordHeader>()
            .ok_or_else(debug_err_fn!(ParseError::Format, "Can't take multicast record header"))?;
        let sources = data
            .take_slice_front::<Ipv6Addr>(header.number_of_sources().into())
            .ok_or_else(debug_err_fn!(ParseError::Format, "Can't take multicast record sources"))?;
        // every record may have aux_data_len 32-bit words at the end.
        // we need to update our buffer view to reflect that.
        let _ = data
            .take_front(usize::from(header.aux_data_len) * 4)
            .ok_or_else(debug_err_fn!(ParseError::Format, "Can't skip auxiliary data"))?;

        Ok(ParsedRecord::Parsed(Self::Record { header, sources }))
    }
}

/// The layout for an MLDv2 message body header, per [RFC 3810 section 5.2].
///
/// [RFC 3810 section 5.2]: https://www.rfc-editor.org/rfc/rfc3810#section-5.2
#[repr(C)]
#[derive(AsBytes, FromZeros, FromBytes, NoCell, Unaligned, Copy, Clone, Debug)]
pub struct Mldv2ReportMessageHeader {
    /// Initialized to zero by the sender; ignored by receivers.
    _reserved: [u8; 2],
    /// The number of multicast address records found in this message.
    num_mcast_addr_records: U16,
}

impl Mldv2ReportMessageHeader {
    /// Returns the number of multicast address records found in this message.
    pub fn num_mcast_addr_records(&self) -> u16 {
        self.num_mcast_addr_records.get()
    }
}

/// The on-wire structure for the body of an MLDv2 report message, per
/// [RFC 3910 section 5.2].
///
/// [RFC 3810 section 5.2]: https://www.rfc-editor.org/rfc/rfc3810#section-5.2
#[derive(Debug)]
pub struct Mldv2ReportBody<B: ByteSlice> {
    header: Ref<B, Mldv2ReportMessageHeader>,
    records: Records<B, Mldv2ReportRecords>,
}

impl<B: ByteSlice> Mldv2ReportBody<B> {
    /// Returns the header.
    pub fn header(&self) -> &Mldv2ReportMessageHeader {
        self.header.deref()
    }

    /// Returns an iterator over the multicast address records.
    pub fn iter_multicast_records(&self) -> impl Iterator<Item = MulticastRecord<&'_ [u8]>> {
        self.records.iter()
    }
}

impl<B: ByteSlice> MessageBody for Mldv2ReportBody<B> {
    type B = B;
    fn parse(bytes: B) -> ParseResult<Self> {
        let (header, bytes) =
            Ref::<_, Mldv2ReportMessageHeader>::new_from_prefix(bytes).ok_or(ParseError::Format)?;
        let records = Records::parse_with_context(bytes, header.num_mcast_addr_records().into())?;
        Ok(Mldv2ReportBody { header, records })
    }

    fn len(&self) -> usize {
        self.bytes().len()
    }

    fn bytes(&self) -> &[u8] {
        self.header.bytes()
    }
}

/// Multicast Listener Report V2 Message.
#[repr(C)]
#[derive(AsBytes, FromZeros, FromBytes, NoCell, Unaligned, Copy, Clone, Debug)]
pub struct MulticastListenerReportV2;

impl_icmp_message!(
    Ipv6,
    MulticastListenerReportV2,
    MulticastListenerReportV2,
    IcmpUnusedCode,
    Mldv2ReportBody<B>
);

/// Multicast Listener Query V1 Message.
#[repr(C)]
#[derive(AsBytes, FromZeros, FromBytes, NoCell, Unaligned, Copy, Clone, Debug)]
pub struct MulticastListenerQuery;

/// Multicast Listener Report V1 Message.
#[repr(C)]
#[derive(AsBytes, FromZeros, FromBytes, NoCell, Unaligned, Copy, Clone, Debug)]
pub struct MulticastListenerReport;

/// Multicast Listener Done V1 Message.
#[repr(C)]
#[derive(AsBytes, FromZeros, FromBytes, NoCell, Unaligned, Copy, Clone, Debug)]
pub struct MulticastListenerDone;

/// MLD Errors.
#[derive(Debug, PartialEq)]
pub enum MldError {
    /// Raised when `MaxRespCode` cannot fit in `u16`.
    MaxRespCodeOverflow,
    /// Raised when a duration cannot be exactly expressed with a MaxRespCode. This can happen when
    /// the exponential mapping from MLDv2 is used. For an example see [RFC 3810 section 5.1].
    /// This error is not used for MLDv1.
    ///
    /// [RFC 3810 section 5.1]: https://datatracker.ietf.org/doc/html/rfc3810#section-5.1
    MaxRespCodeLossyConversion,
}

/// The trait for all MLDv1 Messages.
pub trait Mldv1MessageType {
    /// The type used to represent maximum response delay.
    ///
    /// It should be `()` for Report and Done messages,
    /// and be `Mldv1ResponseDelay` for Query messages.
    type MaxRespDelay: MaxCode<U16> + Debug + Copy;
    /// The type used to represent the group_addr in the message.
    ///
    /// For Query Messages, it is just `Ipv6Addr` because
    /// general queries will have this field to be zero, which
    /// is not a multicast address, for Report and Done messages,
    /// this should be `MulticastAddr<Ipv6Addr>`.
    type GroupAddr: Into<Ipv6Addr> + Debug + Copy;
}

/// The trait for all ICMPv6 messages holding MLDv1 messages.
pub trait IcmpMldv1MessageType:
    Mldv1MessageType + IcmpMessage<Ipv6, Code = IcmpUnusedCode>
{
}

/// Creates a bitmask of [n] bits, [n] must be <= 31.
/// E.g. for n = 12 yields 0xFFF.
const fn bitmask(n: u8) -> u32 {
    (1 << n) - 1
}

/// The trait converts a code to a floating point value: in a linear fashion up to [SWITCHPOINT]
/// and then using a floating point representation to allow the conversion of larger values.
/// In MLD and IGMP there are different codes that follow this pattern, e.g. QQIC, ResponseDelay
/// ([RFC 3376 section 4.1], [RFC 3810 section 5.1]), which all convert a code with the following
/// underlying structure:
///
///       0    NUM_EXP_BITS       NUM_MANT_BITS
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |X|      exp      |          mant         |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///
/// This trait simplifies the implementation by providing methods to perform the conversion.
///
/// [RFC 3376 section 4.1]: https://datatracker.ietf.org/doc/html/rfc3376#section-4.1
/// [RFC 3810 section 5.1]: https://datatracker.ietf.org/doc/html/rfc3810#section-5.1
// TODO(https://fxbug.dev/42071006): extract the trait in a common file to use it for for IGMPv3.
pub trait LinExpConversion<C: Debug + core::cmp::PartialEq + Copy + Clone>:
    Into<C> + Copy + Clone
{
    // Specified by Implementors
    /// Number of bits used for the mantissa.
    const NUM_MANT_BITS: u8;
    /// Number of bits used for the exponent.
    const NUM_EXP_BITS: u8;
    /// Perform a lossy conversion from the `C` type.
    /// Not all values in `C` can be exactly represented using the code and they will be rounded to
    /// a code that represents a value close the provided one.
    fn lossy_try_from(value: C) -> Result<Self, MldError>;

    // Provided for Implementors
    /// How much the exponent needs to be incremented when performing the exponential conversion.
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

    /// Converts the provided code to a value: in a linear way until [Self::SWITCHPOINT] and using
    /// a floating representation for larger values.
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

    /// Performs a lossy conversion from [value].
    ///
    /// The function will always succeed for values within the valid range. However, the code might
    /// not exactly represent the provided input. E.g. a value of `MAX_VALUE - 1` cannot be exactly
    /// represented with a corresponding code, due the exponential representation. However, the
    /// function will be able to provide a code representing a value close to the provided one.
    ///
    /// If stronger guarantees are needed consider using [exact_try_from_expanded].
    fn lossy_try_from_expanded(value: u32) -> Result<u16, MldError> {
        if value > Self::MAX_VALUE {
            Err(MldError::MaxRespCodeOverflow)
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

    /// Performs an exact conversion from [value].
    ///
    /// The function will succeed only for values within the valid range that can be exactly
    /// represented by the produced code. E.g. a value of FLOATING_POINT_MAX_VALUE - 1 cannot be
    /// exactly represented with a corresponding, code due the exponential representation. In this
    /// case, the function will return an error.
    ///
    /// If a lossy conversion can be tolerated consider using [lossy_try_from_expanded].
    fn exact_try_from(value: C) -> Result<Self, MldError>
    where
        Self: core::marker::Sized,
    {
        let res = Self::lossy_try_from(value)?;
        if value == res.into() {
            Ok(res)
        } else {
            Err(MldError::MaxRespCodeLossyConversion)
        }
    }
}

/// The trait for MLD codes that can be further interpreted using different methods e.g. QQIC.
///
/// The type implementing this trait should be able
/// to convert itself from/to `T`
pub trait MaxCode<T: Default + Debug + FromBytes + AsBytes> {
    /// Convert to `T`
    #[allow(clippy::wrong_self_convention)]
    fn as_code(self) -> T;

    /// Convert from `T`
    fn from_code(code: T) -> Self;
}

impl<T: Default + Debug + FromBytes + AsBytes> MaxCode<T> for () {
    fn as_code(self) -> T {
        T::default()
    }

    fn from_code(_: T) -> Self {}
}

/// Maximum Response Delay used in Query messages.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub struct Mldv1ResponseDelay(u16);

impl MaxCode<U16> for Mldv1ResponseDelay {
    fn as_code(self) -> U16 {
        U16::new(self.0)
    }

    fn from_code(code: U16) -> Self {
        Mldv1ResponseDelay(code.get())
    }
}

impl From<Mldv1ResponseDelay> for Duration {
    fn from(code: Mldv1ResponseDelay) -> Self {
        Duration::from_millis(code.0.into())
    }
}

impl TryFrom<Duration> for Mldv1ResponseDelay {
    type Error = MldError;
    fn try_from(period: Duration) -> Result<Self, Self::Error> {
        Ok(Mldv1ResponseDelay(
            u16::try_from(period.as_millis()).map_err(|_| MldError::MaxRespCodeOverflow)?,
        ))
    }
}

/// The layout for an MLDv1 message body.
#[repr(C)]
#[derive(AsBytes, FromZeros, FromBytes, NoCell, Unaligned, Copy, Clone, Debug)]
pub struct Mldv1Message {
    /// Max Response Delay, in units of milliseconds.
    pub max_response_delay: U16,
    /// Initialized to zero by the sender; ignored by receivers.
    reserved: U16,
    /// In a Query message, the Multicast Address field is set to zero when
    /// sending a General Query, and set to a specific IPv6 multicast address
    /// when sending a Multicast-Address-Specific Query.
    ///
    /// In a Report or Done message, the Multicast Address field holds a
    /// specific IPv6 multicast address to which the message sender is
    /// listening or is ceasing to listen, respectively.
    pub group_addr: Ipv6Addr,
}

impl Mldv1Message {
    /// Gets the response delay value.
    pub fn max_response_delay(&self) -> Duration {
        Mldv1ResponseDelay(self.max_response_delay.get()).into()
    }
}

/// The on-wire structure for the body of an MLDv1 message.
#[derive(Debug)]
pub struct Mldv1Body<B: ByteSlice>(Ref<B, Mldv1Message>);

impl<B: ByteSlice> Deref for Mldv1Body<B> {
    type Target = Mldv1Message;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl<B: ByteSlice> MessageBody for Mldv1Body<B> {
    type B = B;
    fn parse(bytes: B) -> ParseResult<Self> {
        Ref::new(bytes).map_or(Err(ParseError::Format), |body| Ok(Mldv1Body(body)))
    }

    fn len(&self) -> usize {
        self.bytes().len()
    }

    fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }
}

macro_rules! impl_mldv1_message {
    ($msg:ident, $resp_code:ty, $group_addr:ty) => {
        impl_icmp_message!(Ipv6, $msg, $msg, IcmpUnusedCode, Mldv1Body<B>);
        impl Mldv1MessageType for $msg {
            type MaxRespDelay = $resp_code;
            type GroupAddr = $group_addr;
        }
        impl IcmpMldv1MessageType for $msg {}
    };
}

impl_mldv1_message!(MulticastListenerQuery, Mldv1ResponseDelay, Ipv6Addr);
impl_mldv1_message!(MulticastListenerReport, (), MulticastAddr<Ipv6Addr>);
impl_mldv1_message!(MulticastListenerDone, (), MulticastAddr<Ipv6Addr>);

/// The builder for MLDv1 Messages.
#[derive(Debug)]
pub struct Mldv1MessageBuilder<M: Mldv1MessageType> {
    max_resp_delay: M::MaxRespDelay,
    group_addr: M::GroupAddr,
}

impl<M: Mldv1MessageType<MaxRespDelay = ()>> Mldv1MessageBuilder<M> {
    /// Create an `Mldv1MessageBuilder` without a `max_resp_delay`
    /// for Report and Done messages.
    pub fn new(group_addr: M::GroupAddr) -> Self {
        Mldv1MessageBuilder { max_resp_delay: (), group_addr }
    }
}

impl<M: Mldv1MessageType> Mldv1MessageBuilder<M> {
    /// Create an `Mldv1MessageBuilder` with a `max_resp_delay`
    /// for Query messages.
    pub fn new_with_max_resp_delay(
        group_addr: M::GroupAddr,
        max_resp_delay: M::MaxRespDelay,
    ) -> Self {
        Mldv1MessageBuilder { max_resp_delay, group_addr }
    }

    fn serialize_message(&self, mut buf: &mut [u8]) {
        use packet::BufferViewMut;
        let mut bytes = &mut buf;
        bytes
            .write_obj_front(&Mldv1Message {
                max_response_delay: self.max_resp_delay.as_code(),
                reserved: U16::ZERO,
                group_addr: self.group_addr.into(),
            })
            .expect("too few bytes for MLDv1 message");
    }
}

impl<M: Mldv1MessageType> InnerPacketBuilder for Mldv1MessageBuilder<M> {
    fn bytes_len(&self) -> usize {
        size_of::<Mldv1Message>()
    }

    fn serialize(&self, buf: &mut [u8]) {
        self.serialize_message(buf);
    }
}

/// Maximum Response Delay used in Queryv2 messages, defined in [RFC 3810 section 5.1.3].
/// [RFC 3810 section 5.1.3]: https://datatracker.ietf.org/doc/html/rfc3810#section-5.1.3
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub struct Mldv2ResponseDelay(u16);

impl LinExpConversion<Duration> for Mldv2ResponseDelay {
    const NUM_MANT_BITS: u8 = 12;
    const NUM_EXP_BITS: u8 = 3;

    fn lossy_try_from(value: Duration) -> Result<Self, MldError> {
        let millis: u32 =
            value.as_millis().try_into().map_err(|_| MldError::MaxRespCodeOverflow)?;
        let code = Self::lossy_try_from_expanded(millis)?;
        Ok(Self(code))
    }
}

impl MaxCode<U16> for Mldv2ResponseDelay {
    fn as_code(self) -> U16 {
        U16::new(self.0)
    }

    fn from_code(code: U16) -> Self {
        Mldv2ResponseDelay(code.get())
    }
}

impl From<Mldv2ResponseDelay> for Duration {
    fn from(code: Mldv2ResponseDelay) -> Self {
        Duration::from_millis(Mldv2ResponseDelay::to_expanded(code.0).into())
    }
}

/// QQIC (Querier's Query Interval Code) used in Queryv2 messages, defined in
/// [RFC 3810 section 5.1.9].
/// [RFC 3810 section 5.1.9]: https://datatracker.ietf.org/doc/html/rfc3810#section-5.1.9
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub struct Mldv2QQIC(u8);

impl MaxCode<u8> for Mldv2QQIC {
    fn as_code(self) -> u8 {
        self.0
    }

    fn from_code(code: u8) -> Self {
        Mldv2QQIC(code)
    }
}

impl LinExpConversion<Duration> for Mldv2QQIC {
    const NUM_MANT_BITS: u8 = 4;
    const NUM_EXP_BITS: u8 = 3;

    fn lossy_try_from(value: Duration) -> Result<Self, MldError> {
        let secs: u32 = value.as_secs().try_into().map_err(|_| MldError::MaxRespCodeOverflow)?;
        let code = Self::lossy_try_from_expanded(secs)?
            .try_into()
            .map_err(|_| MldError::MaxRespCodeOverflow)?;
        Ok(Self(code))
    }
}

impl From<Mldv2QQIC> for Duration {
    fn from(code: Mldv2QQIC) -> Self {
        let secs: u64 = Mldv2QQIC::to_expanded(code.0.into()).into();
        Duration::from_secs(secs)
    }
}

#[cfg(test)]
mod tests {

    use packet::{ParseBuffer, Serializer};

    use super::*;
    use crate::icmp::{IcmpPacketBuilder, IcmpParseArgs};
    use crate::ip::Ipv6Proto;
    use crate::ipv6::ext_hdrs::{
        ExtensionHeaderOptionAction, HopByHopOption, HopByHopOptionData, Ipv6ExtensionHeaderData,
    };
    use crate::ipv6::{Ipv6Header, Ipv6Packet, Ipv6PacketBuilder, Ipv6PacketBuilderWithHbhOptions};
    use test_case::test_case;

    fn serialize_to_bytes<B: ByteSlice + Debug, M: IcmpMessage<Ipv6> + Mldv1MessageType + Debug>(
        src_ip: Ipv6Addr,
        dst_ip: Ipv6Addr,
        icmp: &IcmpPacket<Ipv6, B, M>,
    ) -> Vec<u8> {
        let ip = Ipv6PacketBuilder::new(src_ip, dst_ip, 1, Ipv6Proto::Icmpv6);
        let with_options = Ipv6PacketBuilderWithHbhOptions::new(
            ip,
            &[HopByHopOption {
                action: ExtensionHeaderOptionAction::SkipAndContinue,
                mutable: false,
                data: HopByHopOptionData::RouterAlert { data: 0 },
            }],
        )
        .unwrap();
        icmp.message_body
            .bytes()
            .into_serializer()
            .encapsulate(icmp.builder(src_ip, dst_ip))
            .encapsulate(with_options)
            .serialize_vec_outer()
            .unwrap()
            .as_ref()
            .to_vec()
    }

    fn test_parse_and_serialize<
        M: IcmpMessage<Ipv6> + Mldv1MessageType + Debug,
        F: FnOnce(&Ipv6Packet<&[u8]>),
        G: for<'a> FnOnce(&IcmpPacket<Ipv6, &'a [u8], M>),
    >(
        src_ip: Ipv6Addr,
        dst_ip: Ipv6Addr,
        mut req: &[u8],
        check_ip: F,
        check_icmp: G,
    ) {
        let orig_req = req;

        let ip = req.parse_with::<_, Ipv6Packet<_>>(()).unwrap();
        check_ip(&ip);
        let icmp =
            req.parse_with::<_, IcmpPacket<_, _, M>>(IcmpParseArgs::new(src_ip, dst_ip)).unwrap();
        check_icmp(&icmp);

        let data = serialize_to_bytes(src_ip, dst_ip, &icmp);
        assert_eq!(&data[..], orig_req);
    }

    fn serialize_to_bytes_with_builder<M: IcmpMldv1MessageType + Debug>(
        src_ip: Ipv6Addr,
        dst_ip: Ipv6Addr,
        msg: M,
        group_addr: M::GroupAddr,
        max_resp_delay: M::MaxRespDelay,
    ) -> Vec<u8> {
        let ip = Ipv6PacketBuilder::new(src_ip, dst_ip, 1, Ipv6Proto::Icmpv6);
        let with_options = Ipv6PacketBuilderWithHbhOptions::new(
            ip,
            &[HopByHopOption {
                action: ExtensionHeaderOptionAction::SkipAndContinue,
                mutable: false,
                data: HopByHopOptionData::RouterAlert { data: 0 },
            }],
        )
        .unwrap();
        // Serialize an MLD(ICMPv6) packet using the builder.
        Mldv1MessageBuilder::<M>::new_with_max_resp_delay(group_addr, max_resp_delay)
            .into_serializer()
            .encapsulate(IcmpPacketBuilder::new(src_ip, dst_ip, IcmpUnusedCode, msg))
            .encapsulate(with_options)
            .serialize_vec_outer()
            .unwrap()
            .as_ref()
            .to_vec()
    }

    fn check_ip<B: ByteSlice>(ip: &Ipv6Packet<B>, src_ip: Ipv6Addr, dst_ip: Ipv6Addr) {
        assert_eq!(ip.src_ip(), src_ip);
        assert_eq!(ip.dst_ip(), dst_ip);
        assert_eq!(ip.iter_extension_hdrs().count(), 1);
        let hbh = ip.iter_extension_hdrs().next().unwrap();
        match hbh.data() {
            Ipv6ExtensionHeaderData::HopByHopOptions { options } => {
                assert_eq!(options.iter().count(), 1);
                assert_eq!(
                    options.iter().next().unwrap(),
                    HopByHopOption {
                        action: ExtensionHeaderOptionAction::SkipAndContinue,
                        mutable: false,
                        data: HopByHopOptionData::RouterAlert { data: 0 },
                    }
                );
            }
            _ => panic!("Wrong extension header"),
        }
    }

    fn check_icmp<
        B: ByteSlice,
        M: IcmpMessage<Ipv6, Body<B> = Mldv1Body<B>> + Mldv1MessageType + Debug,
    >(
        icmp: &IcmpPacket<Ipv6, B, M>,
        max_resp_code: u16,
        group_addr: Ipv6Addr,
    ) {
        assert_eq!(icmp.message_body.reserved.get(), 0);
        assert_eq!(icmp.message_body.max_response_delay.get(), max_resp_code);
        assert_eq!(icmp.message_body.group_addr, group_addr);
    }

    #[test]
    fn test_mld_parse_and_serialize_query() {
        use crate::icmp::mld::MulticastListenerQuery;
        use crate::testdata::mld_router_query::*;
        test_parse_and_serialize::<MulticastListenerQuery, _, _>(
            SRC_IP,
            DST_IP,
            QUERY,
            |ip| {
                check_ip(ip, SRC_IP, DST_IP);
            },
            |icmp| {
                check_icmp(icmp, MAX_RESP_CODE, HOST_GROUP_ADDRESS);
            },
        );
    }

    #[test]
    fn test_mld_parse_and_serialize_report() {
        use crate::icmp::mld::MulticastListenerReport;
        use crate::testdata::mld_router_report::*;
        test_parse_and_serialize::<MulticastListenerReport, _, _>(
            SRC_IP,
            DST_IP,
            REPORT,
            |ip| {
                check_ip(ip, SRC_IP, DST_IP);
            },
            |icmp| {
                check_icmp(icmp, 0, HOST_GROUP_ADDRESS);
            },
        );
    }

    #[test]
    fn test_mld_parse_and_serialize_done() {
        use crate::icmp::mld::MulticastListenerDone;
        use crate::testdata::mld_router_done::*;
        test_parse_and_serialize::<MulticastListenerDone, _, _>(
            SRC_IP,
            DST_IP,
            DONE,
            |ip| {
                check_ip(ip, SRC_IP, DST_IP);
            },
            |icmp| {
                check_icmp(icmp, 0, HOST_GROUP_ADDRESS);
            },
        );
    }

    #[test]
    fn test_mld_serialize_and_parse_query() {
        use crate::icmp::mld::MulticastListenerQuery;
        use crate::testdata::mld_router_query::*;
        let bytes = serialize_to_bytes_with_builder::<_>(
            SRC_IP,
            DST_IP,
            MulticastListenerQuery,
            HOST_GROUP_ADDRESS,
            Duration::from_secs(1).try_into().unwrap(),
        );
        assert_eq!(&bytes[..], QUERY);
        let mut req = &bytes[..];
        let ip = req.parse_with::<_, Ipv6Packet<_>>(()).unwrap();
        check_ip(&ip, SRC_IP, DST_IP);
        let icmp = req
            .parse_with::<_, IcmpPacket<_, _, MulticastListenerQuery>>(IcmpParseArgs::new(
                SRC_IP, DST_IP,
            ))
            .unwrap();
        check_icmp(&icmp, MAX_RESP_CODE, HOST_GROUP_ADDRESS);
    }

    #[test]
    fn test_mld_serialize_and_parse_report() {
        use crate::icmp::mld::MulticastListenerReport;
        use crate::testdata::mld_router_report::*;
        let bytes = serialize_to_bytes_with_builder::<_>(
            SRC_IP,
            DST_IP,
            MulticastListenerReport,
            MulticastAddr::new(HOST_GROUP_ADDRESS).unwrap(),
            (),
        );
        assert_eq!(&bytes[..], REPORT);
        let mut req = &bytes[..];
        let ip = req.parse_with::<_, Ipv6Packet<_>>(()).unwrap();
        check_ip(&ip, SRC_IP, DST_IP);
        let icmp = req
            .parse_with::<_, IcmpPacket<_, _, MulticastListenerReport>>(IcmpParseArgs::new(
                SRC_IP, DST_IP,
            ))
            .unwrap();
        check_icmp(&icmp, 0, HOST_GROUP_ADDRESS);
    }

    #[test]
    fn test_mld_serialize_and_parse_done() {
        use crate::icmp::mld::MulticastListenerDone;
        use crate::testdata::mld_router_done::*;
        let bytes = serialize_to_bytes_with_builder::<_>(
            SRC_IP,
            DST_IP,
            MulticastListenerDone,
            MulticastAddr::new(HOST_GROUP_ADDRESS).unwrap(),
            (),
        );
        assert_eq!(&bytes[..], DONE);
        let mut req = &bytes[..];
        let ip = req.parse_with::<_, Ipv6Packet<_>>(()).unwrap();
        check_ip(&ip, SRC_IP, DST_IP);
        let icmp = req
            .parse_with::<_, IcmpPacket<_, _, MulticastListenerDone>>(IcmpParseArgs::new(
                SRC_IP, DST_IP,
            ))
            .unwrap();
        check_icmp(&icmp, 0, HOST_GROUP_ADDRESS);
    }

    #[test]
    fn test_mld_parse_report_v2() {
        use crate::icmp::mld::MulticastListenerReportV2;
        use crate::testdata::mld_router_report_v2::*;
        let req = REPORT.to_vec();
        let mut req = &req[..];
        let ip = req.parse_with::<_, Ipv6Packet<_>>(()).unwrap();
        check_ip(&ip, SRC_IP, DST_IP);
        let icmp = req
            .parse_with::<_, IcmpPacket<_, _, MulticastListenerReportV2>>(IcmpParseArgs::new(
                SRC_IP, DST_IP,
            ))
            .unwrap();
        assert_eq!(
            &icmp
                .body()
                .iter_multicast_records()
                .map(|record| {
                    let hdr = record.header();
                    assert_eq!(record.sources(), SOURCES);
                    (hdr.record_type().expect("valid record type"), *hdr.multicast_addr())
                })
                .collect::<Vec<_>>()[..],
            RECORDS,
        );
    }

    #[test]
    fn test_mld_parse_and_serialize_response_delay_v2_linear() {
        // Linear code:duration mapping
        for code in 0..(Mldv2ResponseDelay::SWITCHPOINT as u16) {
            let response_delay = Mldv2ResponseDelay::from_code(U16::from(code));
            let duration = Duration::from(response_delay);
            assert_eq!(duration.as_millis(), code.into());

            let duration = Duration::from_millis(code.into());
            let response_delay_code: u16 =
                Mldv2ResponseDelay::lossy_try_from(duration).unwrap().as_code().into();
            assert_eq!(response_delay_code, code);

            let duration = Duration::from_millis(code.into());
            let response_delay_code: u16 =
                Mldv2ResponseDelay::exact_try_from(duration).unwrap().as_code().into();
            assert_eq!(response_delay_code, code);
        }
    }

    #[test_case(Mldv2ResponseDelay::SWITCHPOINT, 0x8000; "min exponential value")]
    #[test_case(32784,                           0x8002; "exponental value 32784")]
    #[test_case(227744,                          0xABCD; "exponental value 227744")]
    #[test_case(1821184,                         0xDBCA; "exponental value 1821184")]
    #[test_case(8385536,                         0xFFFD; "exponental value 8385536")]
    #[test_case(Mldv2ResponseDelay::MAX_VALUE,   0xFFFF; "max exponential value")]
    fn test_mld_parse_and_serialize_response_delay_v2_exponential_exact(
        duration_millis: u32,
        resp_code: u16,
    ) {
        let response_delay = Mldv2ResponseDelay::from_code(resp_code.into());
        let duration = Duration::from(response_delay);
        assert_eq!(duration.as_millis(), duration_millis.into());

        let response_delay_code: u16 =
            Mldv2ResponseDelay::lossy_try_from(duration).unwrap().as_code().into();
        assert_eq!(response_delay_code, resp_code);

        let response_delay_code: u16 =
            Mldv2ResponseDelay::exact_try_from(duration).unwrap().as_code().into();
        assert_eq!(response_delay_code, resp_code);
    }

    #[test]
    fn test_mld_parse_and_serialize_response_delay_v2_errors() {
        let duration = Duration::from_millis((Mldv2ResponseDelay::MAX_VALUE + 1).into());
        assert_eq!(
            Mldv2ResponseDelay::lossy_try_from(duration),
            Err(MldError::MaxRespCodeOverflow)
        );

        let duration = Duration::from_millis((Mldv2ResponseDelay::MAX_VALUE + 1).into());
        assert_eq!(
            Mldv2ResponseDelay::exact_try_from(duration),
            Err(MldError::MaxRespCodeOverflow)
        );

        let duration = Duration::from_millis((Mldv2ResponseDelay::MAX_VALUE - 1).into());
        assert_eq!(
            Mldv2ResponseDelay::exact_try_from(duration),
            Err(MldError::MaxRespCodeLossyConversion)
        );
    }

    #[test]
    fn test_mld_parse_and_serialize_response_qqic_v2_linear() {
        // Linear code:duration mapping
        for code in 0..(Mldv2QQIC::SWITCHPOINT as u8) {
            let response_delay = Mldv2QQIC::from_code(code);
            let duration = Duration::from(response_delay);
            assert_eq!(duration.as_secs(), code.into());

            let duration = Duration::from_secs(code.into());
            let response_delay_code: u8 =
                Mldv2QQIC::lossy_try_from(duration).unwrap().as_code().into();
            assert_eq!(response_delay_code, code);

            let duration = Duration::from_secs(code.into());
            let response_delay_code: u8 =
                Mldv2QQIC::exact_try_from(duration).unwrap().as_code().into();
            assert_eq!(response_delay_code, code);
        }
    }

    #[test_case(Mldv2QQIC::SWITCHPOINT, 0x80; "min exponential value")]
    #[test_case(144,                    0x82; "exponental value 144")]
    #[test_case(928,                    0xAD; "exponental value 928")]
    #[test_case(6656,                   0xDA; "exponental value 6656")]
    #[test_case(29696,                  0xFD; "exponental value 29696")]
    #[test_case(Mldv2QQIC::MAX_VALUE,   0xFF; "max exponential value")]
    fn test_mld_parse_and_serialize_response_qqic_v2_exponential_exact(
        duration_secs: u32,
        resp_code: u8,
    ) {
        let response_delay = Mldv2QQIC::from_code(resp_code.into());
        let duration = Duration::from(response_delay);
        assert_eq!(duration.as_secs(), duration_secs.into());

        let response_delay_code: u8 = Mldv2QQIC::lossy_try_from(duration).unwrap().as_code().into();
        assert_eq!(response_delay_code, resp_code);

        let response_delay_code: u8 = Mldv2QQIC::exact_try_from(duration).unwrap().as_code().into();
        assert_eq!(response_delay_code, resp_code);
    }

    #[test]
    fn test_mld_parse_and_serialize_response_qqic_v2_errors() {
        let duration = Duration::from_secs((Mldv2QQIC::MAX_VALUE + 1).into());
        assert_eq!(Mldv2QQIC::lossy_try_from(duration), Err(MldError::MaxRespCodeOverflow));

        let duration = Duration::from_secs((Mldv2QQIC::MAX_VALUE + 1).into());
        assert_eq!(Mldv2QQIC::exact_try_from(duration), Err(MldError::MaxRespCodeOverflow));

        let duration = Duration::from_secs((Mldv2QQIC::MAX_VALUE - 1).into());
        assert_eq!(Mldv2QQIC::exact_try_from(duration), Err(MldError::MaxRespCodeLossyConversion));
    }
}
