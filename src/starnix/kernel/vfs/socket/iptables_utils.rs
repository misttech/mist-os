// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains translation between fuchsia.net.filter data structures and Linux
// iptables structures.

use fidl_fuchsia_net_filter_ext::{
    Domain, InstalledIpRoutine, InstalledNatRoutine, IpHook, Namespace, NamespaceId, NatHook,
    Routine, RoutineId, RoutineType, Rule,
};
use starnix_logging::log_warn;
use starnix_uapi::{
    c_char, c_int, c_uint, ipt_entry, ipt_replace,
    xt_entry_match__bindgen_ty_1__bindgen_ty_1 as xt_entry_match,
    xt_entry_target__bindgen_ty_1__bindgen_ty_1 as xt_entry_target, xt_tcp, xt_udp,
};
use std::any::type_name;
use std::ffi::{CStr, CString, NulError};
use std::mem::size_of;
use std::str::Utf8Error;
use thiserror::Error;
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

const TABLE_NAT: &str = "nat";
const CHAIN_PREROUTING: &str = "PREROUTING";
const CHAIN_INPUT: &str = "INPUT";
const CHAIN_FORWARD: &str = "FORWARD";
const CHAIN_OUTPUT: &str = "OUTPUT";
const CHAIN_POSTROUTING: &str = "POSTROUTING";

const IPT_REPLACE_SIZE: usize = size_of::<ipt_replace>();

#[derive(Debug, Error, PartialEq)]
pub enum IpTableParseError {
    #[error("error during ascii conversion: {0}")]
    AsciiConversion(AsciiConversionError),
    #[error("buffer of size {size} is too small to read ipt_replace")]
    BufferTooSmall { size: usize },
    #[error("specified size {specified_size} does not match size of entries {entries_size}")]
    SizeMismatch { specified_size: usize, entries_size: usize },
    #[error("reached end of buffer while trying to parse {type_name} at position {position}")]
    ParseEndOfBuffer { type_name: &'static str, position: usize },
    #[error("reached end of buffer while advancing by {offset} at position {position}")]
    AdvanceEndOfBuffer { offset: usize, position: usize },
    #[error("target offset {offset} is too small to fit ipt_entry")]
    TargetOffsetTooSmall { offset: usize },
    #[error("matchers extend beyond target offset {offset}")]
    InvalidTargetOffset { offset: usize },
    #[error("next offset {offset} is too small to fit ipt_entry")]
    NextOffsetTooSmall { offset: usize },
    #[error("target extends beyond next offset {offset}")]
    InvalidNextOffset { offset: usize },
    #[error("match size {size} is too small to fit xt_entry_match")]
    MatchSizeTooSmall { size: usize },
    #[error("target size {size} does not match specified {match_name} matcher")]
    MatchSizeMismatch { size: usize, match_name: &'static str },
    #[error("target size {size} is too small to fit xt_entry_target")]
    TargetSizeTooSmall { size: usize },
    #[error("target size {size} does not match specified {target_name} target")]
    TargetSizeMismatch { size: usize, target_name: &'static str },
    #[error("specified {specified} entries but found {found} entries")]
    NumEntriesMismatch { specified: usize, found: usize },
    #[error("error entry has unexpected matchers")]
    ErrorEntryHasMatchers,
    #[error("table definition does not have trailing error target")]
    NoTrailingErrorTarget,
}

#[derive(Debug, Error, PartialEq)]
pub enum AsciiConversionError {
    #[error("nul byte not found in ASCII string {chars:?}")]
    NulByteNotFound { chars: Vec<c_char> },
    #[error("unexpected nul byte found in UTF-8 String {0:?}")]
    NulByteInString(NulError),
    #[error("char is out of range for ASCII (0 to 127)")]
    NonAsciiChar,
    #[error("UTF-8 parse error: {0}")]
    Utf8(Utf8Error),
    #[error("buffer of size {buffer_size} too small to fit data of size {data_size}")]
    BufferTooSmall { buffer_size: usize, data_size: usize },
}

#[derive(Debug)]
pub struct IptReplace {
    pub name: String,
    pub num_entries: usize,
    pub size: usize,

    /// Unsupported fields, saved as the same type as `ipt_replace`.
    pub valid_hooks: u32,
    pub hook_entry: [c_uint; 5usize],
    pub underflow: [c_uint; 5usize],
    pub num_counters: c_uint,
}

impl TryFrom<ipt_replace> for IptReplace {
    type Error = IpTableParseError;

    fn try_from(ipt_replace: ipt_replace) -> Result<Self, Self::Error> {
        let name =
            ascii_to_string(&ipt_replace.name).map_err(IpTableParseError::AsciiConversion)?;
        Ok(Self {
            name,
            num_entries: usize::try_from(ipt_replace.num_entries).unwrap(),
            size: usize::try_from(ipt_replace.size).unwrap(),
            valid_hooks: ipt_replace.valid_hooks,
            hook_entry: ipt_replace.hook_entry,
            underflow: ipt_replace.underflow,
            num_counters: ipt_replace.num_counters,
        })
    }
}

/// An "Entry" is either:
///
/// 1. Start of a new iptables chain
/// 2. A rule on the chain
/// 3. The policy of the chain
/// 4. End of input
#[derive(Debug)]
pub struct Entry {
    /// bytes since the first entry, referred to by JUMP targets.
    pub byte_pos: usize,

    pub ipt_entry: ipt_entry,
    pub matchers: Vec<Matcher>,
    pub target: Target,
}

#[derive(Debug)]
pub enum Matcher {
    Unknown { name: String, bytes: Vec<u8> },
    Tcp(xt_tcp),
    Udp(xt_udp),
}

#[derive(Debug)]
pub enum Target {
    Unknown { name: String, bytes: Vec<u8> },
    Standard(c_int),
    Error(String),
}

// `xt_standard_target` without the `target` field.
//
// `target` of type `xt_entry_target` is parsed first to determine the target's variant.
#[repr(C)]
#[derive(AsBytes, Debug, Default, FromBytes, FromZeros, NoCell)]
struct VerdictWithPadding {
    pub verdict: c_int,
    pub _padding: [u8; 4usize],
}

// `xt_error_target` without the `target` field.
//
// `target` of type `xt_entry_target` is parsed first to determine the target's variant.
#[repr(C)]
#[derive(AsBytes, Debug, Default, FromBytes, FromZeros, NoCell)]
struct ErrorNameWithPadding {
    pub errorname: [c_char; 30usize],
    pub _padding: [u8; 2usize],
}

#[derive(Debug)]
pub struct IptReplaceParser {
    pub replace_info: IptReplace,

    // Linux bytes to parse.
    //
    // General layout is an `ipt_replace` followed by N "entries", where each "entry" is
    // an `ipt_entry` and a `xt_*_target` with 0 or more "matchers" in between.
    //
    // In this example, each row after the first is an entry:
    //
    //        [ ipt_replace ]
    //   0:   [ ipt_entry ][ xt_error_target ]
    //   1:   [ ipt_entry ][ xt_entry_match ][ xt_tcp ] ... [ xt_standard_target ]
    //   2:   [ ipt_entry ][ xt_error_target ]
    //        ...
    //   N-1: [ ipt_entry ][ xt_error_target ]
    bytes: Vec<u8>,

    parse_pos: usize,
}

impl IptReplaceParser {
    /// Initialize a new parser and tries to parse an `ipt_replace` struct from the buffer.
    /// The rest of the buffer is left unparsed.
    fn new(bytes: Vec<u8>) -> Result<Self, IpTableParseError> {
        if bytes.len() < IPT_REPLACE_SIZE {
            return Err(IpTableParseError::BufferTooSmall { size: bytes.len() });
        }

        let ipt_replace = ipt_replace::read_from(&bytes[..IPT_REPLACE_SIZE])
            .expect("successfully read ipt_replace");
        let replace_info = IptReplace::try_from(ipt_replace)?;

        if replace_info.size != bytes.len() - IPT_REPLACE_SIZE {
            return Err(IpTableParseError::SizeMismatch {
                specified_size: replace_info.size,
                entries_size: bytes.len() - IPT_REPLACE_SIZE,
            });
        }

        Ok(Self { replace_info, bytes, parse_pos: IPT_REPLACE_SIZE })
    }

    fn finished(&self) -> bool {
        self.parse_pos == self.bytes.len()
    }

    pub fn entries_bytes(&self) -> &[u8] {
        &self.bytes[IPT_REPLACE_SIZE..]
    }

    fn bytes_since_first_entry(&self) -> usize {
        self.parse_pos
            .checked_sub(IPT_REPLACE_SIZE)
            .expect("parse_pos starts after initial ipt_replace")
    }

    fn get_next_bytes(&self, offset: usize) -> Option<&[u8]> {
        let new_pos = self.parse_pos + offset;
        if new_pos > self.bytes.len() {
            None
        } else {
            Some(&self.bytes[self.parse_pos..new_pos])
        }
    }

    // Parse `bytes` starting from `parse_pos` as type T, without advancing `parse_pos`.
    // Used in cases where part of a structure must be parsed first, before determining how to parse
    // the rest of the structure.
    fn view_next_bytes_as<T: FromBytes>(&self) -> Result<T, IpTableParseError> {
        let bytes = self.get_next_bytes(size_of::<T>()).ok_or_else(|| {
            IpTableParseError::ParseEndOfBuffer {
                type_name: type_name::<T>(),
                position: self.parse_pos,
            }
        })?;
        let obj = T::read_from(bytes).expect("read_from slice of exact size is successful");
        Ok(obj)
    }

    // Add `offset` to `parse_pos`. Should be used after `view_next_bytes_as`.
    fn advance_parse_pos(&mut self, offset: usize) -> Result<(), IpTableParseError> {
        if self.parse_pos + offset > self.bytes.len() {
            return Err(IpTableParseError::AdvanceEndOfBuffer { offset, position: self.parse_pos });
        }
        self.parse_pos += offset;
        Ok(())
    }

    // Parse `bytes` starting from `parse_pos` as type T, and advance `parse_pos`.
    fn parse_next_bytes_as<T: FromBytes>(&mut self) -> Result<T, IpTableParseError> {
        let obj = self.view_next_bytes_as::<T>()?;
        self.advance_parse_pos(size_of::<T>())?;
        Ok(obj)
    }

    /// Parse next bytes as an `ipt_entry` struct, its subsequent matchers and target.
    fn parse_entry(&mut self) -> Result<Entry, IpTableParseError> {
        let byte_pos = self.bytes_since_first_entry();
        let entry_info = self.parse_next_bytes_as::<ipt_entry>()?;

        let target_offset = usize::from(entry_info.target_offset);
        if target_offset < size_of::<ipt_entry>() {
            return Err(IpTableParseError::TargetOffsetTooSmall { offset: target_offset });
        }
        let target_pos = byte_pos + target_offset;

        let next_offset = usize::from(entry_info.next_offset);
        if next_offset < size_of::<ipt_entry>() {
            return Err(IpTableParseError::NextOffsetTooSmall { offset: next_offset });
        }
        let next_pos = byte_pos + next_offset;

        let mut matchers: Vec<Matcher> = vec![];

        // Each entry has 0 or more matchers.
        while self.bytes_since_first_entry() < target_pos {
            matchers.push(self.parse_matcher()?);
        }

        // Check if matchers extend beyond the target_offset.
        if self.bytes_since_first_entry() != target_pos {
            return Err(IpTableParseError::InvalidTargetOffset { offset: target_offset });
        }

        // Each entry has 1 target.
        let target = self.parse_target()?;

        if self.bytes_since_first_entry() != next_pos {
            return Err(IpTableParseError::InvalidNextOffset { offset: target_offset });
        }

        Ok(Entry { byte_pos, ipt_entry: entry_info, matchers, target })
    }

    // Parses next bytes as a `xt_entry_match` struct and a specified matcher struct.
    fn parse_matcher(&mut self) -> Result<Matcher, IpTableParseError> {
        let match_info = self.parse_next_bytes_as::<xt_entry_match>()?;

        let match_size = usize::from(match_info.match_size);
        if match_size < size_of::<xt_entry_match>() {
            return Err(IpTableParseError::MatchSizeTooSmall { size: match_size });
        }
        let remaining_size = match_size - size_of::<xt_entry_match>();

        let matcher = match ascii_to_string(&match_info.name)
            .map_err(IpTableParseError::AsciiConversion)?
            .as_str()
        {
            "tcp" => {
                if remaining_size < size_of::<xt_tcp>() {
                    return Err(IpTableParseError::MatchSizeMismatch {
                        size: match_size,
                        match_name: "tcp",
                    });
                }
                let tcp = self.view_next_bytes_as::<xt_tcp>()?;
                Matcher::Tcp(tcp)
            }

            "udp" => {
                if remaining_size < size_of::<xt_udp>() {
                    return Err(IpTableParseError::MatchSizeMismatch {
                        size: match_size,
                        match_name: "udp",
                    });
                }
                let udp = self.view_next_bytes_as::<xt_udp>()?;
                Matcher::Udp(udp)
            }

            matcher_name => {
                log_warn!("IpTables: ignored {matcher_name} matcher of size {match_size}");
                let bytes = self
                    .get_next_bytes(remaining_size)
                    .ok_or_else(|| IpTableParseError::MatchSizeMismatch {
                        size: match_size,
                        match_name: "unknown",
                    })?
                    .to_vec();
                Matcher::Unknown { name: matcher_name.to_owned(), bytes }
            }
        };

        // Advance by `remaining_size` to account for padding and unsupported match extensions.
        self.advance_parse_pos(remaining_size)?;
        Ok(matcher)
    }

    // Parses next bytes as a `xt_entry_target` struct and a specified target struct.
    fn parse_target(&mut self) -> Result<Target, IpTableParseError> {
        let target_info = self.parse_next_bytes_as::<xt_entry_target>()?;

        let target_size = usize::from(target_info.target_size);
        if target_size < size_of::<xt_entry_target>() {
            return Err(IpTableParseError::TargetSizeTooSmall { size: target_size });
        }
        let remaining_size = target_size - size_of::<xt_entry_target>();

        let target = match ascii_to_string(&target_info.name)
            .map_err(IpTableParseError::AsciiConversion)?
            .as_str()
        {
            "" => {
                if remaining_size < size_of::<VerdictWithPadding>() {
                    return Err(IpTableParseError::TargetSizeMismatch {
                        size: target_size,
                        target_name: "standard",
                    });
                }
                let standard_target = self.view_next_bytes_as::<VerdictWithPadding>()?;
                Target::Standard(standard_target.verdict)
            }

            "ERROR" => {
                if remaining_size < size_of::<ErrorNameWithPadding>() {
                    return Err(IpTableParseError::TargetSizeMismatch {
                        size: target_size,
                        target_name: "error",
                    });
                }
                let error_target = self.view_next_bytes_as::<ErrorNameWithPadding>()?;
                let errorname = ascii_to_string(&error_target.errorname)
                    .map_err(IpTableParseError::AsciiConversion)?;
                Target::Error(errorname)
            }

            target_name => {
                log_warn!("IpTables: ignored {target_name} target of size {target_size}");
                let bytes = self
                    .get_next_bytes(remaining_size)
                    .ok_or_else(|| IpTableParseError::TargetSizeMismatch {
                        size: target_size,
                        target_name: "unknown",
                    })?
                    .to_vec();
                Target::Unknown { name: target_name.to_owned(), bytes }
            }
        };

        // Advance by `remaining_size` to account for padding and unsupported target extensions.
        self.advance_parse_pos(remaining_size)?;
        Ok(target)
    }
}

#[derive(Debug)]
pub struct IpTable {
    /// The parser used to translate Linux data into fuchsia.net.filter resources.
    /// Included here as we don't have the reverse translation implemented yet.
    /// TODO(b/307908515): Remove once we can recreate Linux structure from net filter resources.
    pub parser: IptReplaceParser,

    /// `namespace`, `routines` and `rules` make up an IPTable's representation
    /// in fuchsia.net.filter's API, where Namespace stores metadata about the table
    /// like its name, Routine correspond to a chain on the table, and Rule is a rule
    /// on a chain. We can update the table state of the system by dropping the Namespace,
    /// then recreating the Namespace, Routines, and Rules in that order.
    pub namespace: Namespace,
    pub routines: Vec<Routine>,
    pub rules: Vec<Rule>,
}

impl IpTable {
    pub fn from_ipt_replace(bytes: Vec<u8>) -> Result<Self, IpTableParseError> {
        let mut parser = IptReplaceParser::new(bytes)?;
        let table_name = parser.replace_info.name.clone();

        let mut entries: Vec<Entry> = vec![];

        while !parser.finished() {
            entries.push(parser.parse_entry()?);
        }

        if entries.len() != parser.replace_info.num_entries {
            return Err(IpTableParseError::NumEntriesMismatch {
                specified: parser.replace_info.num_entries,
                found: entries.len(),
            });
        }

        let routines = create_routines(&table_name, &entries)?;

        Ok(IpTable {
            parser,
            namespace: Namespace { id: NamespaceId(table_name), domain: Domain::Ipv4 },
            routines: routines,
            // TODO(b/307908515): Create Rules from entries.
            rules: vec![],
        })
    }
}

fn get_routine_type(table_name: &str, chain_name: &str) -> RoutineType {
    match (table_name, chain_name) {
        (TABLE_NAT, CHAIN_PREROUTING) => {
            RoutineType::Nat(Some(InstalledNatRoutine { hook: NatHook::Ingress, priority: 0 }))
        }
        (TABLE_NAT, CHAIN_INPUT) => {
            RoutineType::Nat(Some(InstalledNatRoutine { hook: NatHook::LocalIngress, priority: 0 }))
        }
        (TABLE_NAT, CHAIN_OUTPUT) => {
            RoutineType::Nat(Some(InstalledNatRoutine { hook: NatHook::LocalEgress, priority: 0 }))
        }
        (TABLE_NAT, CHAIN_POSTROUTING) => {
            RoutineType::Nat(Some(InstalledNatRoutine { hook: NatHook::Egress, priority: 0 }))
        }
        (TABLE_NAT, _) => RoutineType::Nat(None),
        (_, CHAIN_PREROUTING) => {
            RoutineType::Ip(Some(InstalledIpRoutine { hook: IpHook::Ingress, priority: 0 }))
        }
        (_, CHAIN_INPUT) => {
            RoutineType::Ip(Some(InstalledIpRoutine { hook: IpHook::LocalIngress, priority: 0 }))
        }
        (_, CHAIN_FORWARD) => {
            RoutineType::Ip(Some(InstalledIpRoutine { hook: IpHook::Forwarding, priority: 0 }))
        }
        (_, CHAIN_OUTPUT) => {
            RoutineType::Ip(Some(InstalledIpRoutine { hook: IpHook::LocalEgress, priority: 0 }))
        }
        (_, CHAIN_POSTROUTING) => {
            RoutineType::Ip(Some(InstalledIpRoutine { hook: IpHook::Egress, priority: 0 }))
        }
        (_, _) => RoutineType::Ip(None),
    }
}

fn create_routines(
    table_name: &String,
    entries: &Vec<Entry>,
) -> Result<Vec<Routine>, IpTableParseError> {
    let last_entry = entries.last().ok_or_else(|| IpTableParseError::NoTrailingErrorTarget)?;
    if let Target::Error(chain_name) = &last_entry.target {
        if chain_name.as_str() != "ERROR" {
            return Err(IpTableParseError::NoTrailingErrorTarget);
        }
    } else {
        return Err(IpTableParseError::NoTrailingErrorTarget);
    }

    let mut routines: Vec<Routine> = vec![];
    for entry in entries[..entries.len() - 1].iter() {
        if let Target::Error(chain_name) = &entry.target {
            if !entry.matchers.is_empty() {
                return Err(IpTableParseError::ErrorEntryHasMatchers);
            }

            let routine_type = get_routine_type(table_name.as_str(), chain_name.as_str());

            routines.push(Routine {
                id: RoutineId {
                    namespace: NamespaceId(table_name.clone()),
                    name: chain_name.clone(),
                },
                routine_type: routine_type,
            });
        }
    }
    Ok(routines)
}

// On x86_64, `c_char` is `i8`; try to convert them to `u8`.
// Errors if any character is not in ASCII range (0-127).
#[cfg(target_arch = "x86_64")]
fn ascii_to_bytes(chars: &[c_char]) -> Result<Vec<u8>, AsciiConversionError> {
    chars.iter().map(|&c| u8::try_from(c).map_err(|_| AsciiConversionError::NonAsciiChar)).collect()
}

// On aarch64 and riscv64, `c_char` is already `u8`.
// Errors if any character is not in ASCII range (0-127).
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn ascii_to_bytes(chars: &[c_char]) -> Result<Vec<u8>, AsciiConversionError> {
    if chars.iter().any(|&c| c > 127) {
        return Err(AsciiConversionError::NonAsciiChar);
    }
    Ok(chars.to_owned())
}

fn ascii_to_string(chars: &[c_char]) -> Result<String, AsciiConversionError> {
    let bytes = ascii_to_bytes(chars)?;
    let c_str = CStr::from_bytes_until_nul(&bytes)
        .map_err(|_| AsciiConversionError::NulByteNotFound { chars: chars.to_vec() })?;
    c_str.to_str().map_err(AsciiConversionError::Utf8).map(|s| s.to_owned())
}

// On x86_64, `c_char` is `i8`; try to convert from `u8`.
// Errors if any character is not in ASCII range (0-127), or if `bytes` does not fit inside
// `buffer`.
#[cfg(target_arch = "x86_64")]
fn write_bytes_to_ascii_buffer(
    bytes: &[u8],
    buffer: &mut [c_char],
) -> Result<(), AsciiConversionError> {
    if bytes.len() > buffer.len() {
        return Err(AsciiConversionError::BufferTooSmall {
            buffer_size: buffer.len(),
            data_size: bytes.len(),
        });
    }
    for (idx, elem) in buffer.iter_mut().enumerate() {
        if let Some(&byte) = bytes.get(idx) {
            *elem = i8::try_from(byte).map_err(|_| AsciiConversionError::NonAsciiChar)?;
        } else {
            break;
        }
    }
    Ok(())
}

// On aarch64 and riscv64, `c_char` is already `u8`.
// Errors if any character is not in ASCII range (0-127), or if `bytes` does not fit inside
// `buffer`.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn write_bytes_to_ascii_buffer(
    bytes: &[u8],
    buffer: &mut [c_char],
) -> Result<(), AsciiConversionError> {
    if bytes.len() > buffer.len() {
        return Err(AsciiConversionError::BufferTooSmall {
            buffer_size: buffer.len(),
            data_size: bytes.len(),
        });
    }
    if bytes.iter().any(|&c| c > 127) {
        return Err(AsciiConversionError::NonAsciiChar);
    }
    let dest = &mut buffer[..bytes.len()];
    dest.copy_from_slice(bytes);
    Ok(())
}

pub fn write_string_to_ascii_buffer(
    string: String,
    chars: &mut [c_char],
) -> Result<(), AsciiConversionError> {
    let c_string = CString::new(string).map_err(AsciiConversionError::NulByteInString)?;
    let bytes = c_string.to_bytes_with_nul();
    write_bytes_to_ascii_buffer(bytes, chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use starnix_uapi::{
        c_char, ipt_entry, ipt_replace,
        xt_entry_match__bindgen_ty_1__bindgen_ty_1 as xt_entry_match,
        xt_entry_target__bindgen_ty_1__bindgen_ty_1 as xt_entry_target, xt_tcp, xt_udp,
    };
    use test_case::test_case;

    fn string_to_29_chars(string: &str) -> [c_char; 29] {
        let mut buffer = [0; 29];
        write_string_to_ascii_buffer(String::from(string), &mut buffer).unwrap();
        buffer
    }

    fn string_to_30_chars(string: &str) -> [c_char; 30] {
        let mut buffer = [0; 30];
        write_string_to_ascii_buffer(String::from(string), &mut buffer).unwrap();
        buffer
    }

    fn string_to_32_chars(string: &str) -> [c_char; 32] {
        let mut buffer = [0; 32];
        write_string_to_ascii_buffer(String::from(string), &mut buffer).unwrap();
        buffer
    }

    fn table_with_rules() -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![];

        bytes.extend_from_slice(
            ipt_replace {
                name: string_to_32_chars("filter"),
                num_entries: 5,
                size: 904,
                ..Default::default()
            }
            .as_bytes(),
        );

        // Entry 1: start of the chain.
        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 176, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: 64, name: string_to_29_chars("ERROR"), revision: 0 }
                .as_bytes(),
        );
        bytes.extend_from_slice(
            ErrorNameWithPadding { errorname: string_to_30_chars("mychain"), ..Default::default() }
                .as_bytes(),
        );

        // Entry 2: a rule on the chain.
        bytes.extend_from_slice(
            ipt_entry { target_offset: 160, next_offset: 200, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_match { match_size: 48, name: string_to_29_chars("tcp"), revision: 0 }
                .as_bytes(),
        );
        bytes.extend_from_slice(xt_tcp::default().as_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(
            xt_entry_target { target_size: 40, ..Default::default() }.as_bytes(),
        );
        bytes
            .extend_from_slice(VerdictWithPadding { verdict: -5, ..Default::default() }.as_bytes());

        // Entry 3: another rule on the chain.
        bytes.extend_from_slice(
            ipt_entry { target_offset: 160, next_offset: 200, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_match { match_size: 48, name: string_to_29_chars("udp"), revision: 0 }
                .as_bytes(),
        );
        bytes.extend_from_slice(xt_udp::default().as_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(
            xt_entry_target { target_size: 40, ..Default::default() }.as_bytes(),
        );
        bytes
            .extend_from_slice(VerdictWithPadding { verdict: -5, ..Default::default() }.as_bytes());

        // Entry 4: "policy" of the chain.
        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 152, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: 40, ..Default::default() }.as_bytes(),
        );
        bytes
            .extend_from_slice(VerdictWithPadding { verdict: -5, ..Default::default() }.as_bytes());

        // Entry 5: end of input.
        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 176, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: 64, name: string_to_29_chars("ERROR"), revision: 0 }
                .as_bytes(),
        );
        bytes.extend_from_slice(
            ErrorNameWithPadding { errorname: string_to_30_chars("ERROR"), ..Default::default() }
                .as_bytes(),
        );

        bytes
    }

    #[fuchsia::test]
    fn parse_table_with_one_chain_test() {
        let table = IpTable::from_ipt_replace(table_with_rules()).unwrap();
        assert_eq!(table.namespace.id.0, "filter");
        assert_eq!(table.routines.len(), 1);

        let routine_id = &table.routines.first().as_ref().unwrap().id;
        assert_eq!(routine_id.name, "mychain");
        assert_eq!(routine_id.namespace.0, "filter");
    }

    fn table_with_wrong_size() -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![];

        bytes.extend_from_slice(
            ipt_replace {
                name: string_to_32_chars("filter"),
                num_entries: 1,
                size: 0,
                ..Default::default()
            }
            .as_bytes(),
        );

        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 176, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: 64, name: string_to_29_chars("ERROR"), revision: 0 }
                .as_bytes(),
        );
        bytes.extend_from_slice(
            ErrorNameWithPadding { errorname: string_to_30_chars("ERROR"), ..Default::default() }
                .as_bytes(),
        );

        bytes
    }

    fn table_with_wrong_num_entries() -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![];

        bytes.extend_from_slice(
            ipt_replace {
                name: string_to_32_chars("filter"),
                num_entries: 3,
                size: 176,
                ..Default::default()
            }
            .as_bytes(),
        );

        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 176, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: 64, name: string_to_29_chars("ERROR"), revision: 0 }
                .as_bytes(),
        );
        bytes.extend_from_slice(
            ErrorNameWithPadding { errorname: string_to_30_chars("ERROR"), ..Default::default() }
                .as_bytes(),
        );

        bytes
    }

    fn table_with_no_entries() -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![];

        bytes.extend_from_slice(
            ipt_replace {
                name: string_to_32_chars("filter"),
                num_entries: 0,
                size: 0,
                ..Default::default()
            }
            .as_bytes(),
        );

        bytes
    }

    #[test_case(
        table_with_wrong_size(),
        IpTableParseError::SizeMismatch {
            specified_size: 0,
            entries_size: 176,
        };
        "wrong size"
    )]
    #[test_case(
        table_with_wrong_num_entries(),
        IpTableParseError::NumEntriesMismatch {
            specified: 3,
            found: 1,
        };
        "wrong number of entries"
    )]
    #[test_case(
        table_with_no_entries(),
        IpTableParseError::NoTrailingErrorTarget;
        "no trailing error target"
    )]
    fn parse_table_error(bytes: Vec<u8>, expected_error: IpTableParseError) {
        assert_eq!(IpTable::from_ipt_replace(bytes).unwrap_err(), expected_error);
    }

    #[test_case(&[], Err(AsciiConversionError::NulByteNotFound { chars: vec![] }); "empty slice")]
    #[test_case(&[0], Ok(String::from("")); "size 1 slice")]
    #[test_case(
        &[102, 105, 108, 116, 101, 114, 0],
        Ok(String::from("filter"));
        "valid string with trailing nul byte"
    )]
    #[test_case(
        &[102, 105, 108, 116, 101, 114, 0, 0, 0, 0],
        Ok(String::from("filter"));
        "multiple trailing nul bytes"
    )]
    #[test_case(&[0; 8], Ok(String::from("")); "empty string")]
    #[test_case(&[0, 88, 88, 88, 88, 88], Ok(String::from("")); "ignores chars after nul byte")]
    fn ascii_to_string_test(input: &[c_char], expected: Result<String, AsciiConversionError>) {
        assert_eq!(ascii_to_string(input), expected);
    }

    #[fuchsia::test]
    fn ascii_to_string_non_ascii_test() {
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        {
            let invalid_bytes: [c_char; 4] = [159, 146, 150, 0];
            assert_eq!(ascii_to_string(&invalid_bytes), Err(AsciiConversionError::NonAsciiChar));
        }
        #[cfg(target_arch = "x86_64")]
        {
            let invalid_bytes: [c_char; 4] = [-97, -110, -106, 0];
            assert_eq!(ascii_to_string(&invalid_bytes), Err(AsciiConversionError::NonAsciiChar));
        }
    }

    #[test_case(String::from(""), Ok(()), [0, 0, 0, 0, 0, 0, 0, 0]; "empty string")]
    #[test_case(
        String::from("filter"),
        Ok(()),
        [102, 105, 108, 116, 101, 114, 0, 0];
        "valid string"
    )]
    #[test_case(
        String::from("very long string"),
        Err(AsciiConversionError::BufferTooSmall { buffer_size: 8, data_size: 17 }),
        [0, 0, 0, 0, 0, 0, 0, 0];
        "string does not fit"
    )]
    #[test_case(
        String::from("\u{211D}"),
        Err(AsciiConversionError::NonAsciiChar),
        [0, 0, 0, 0, 0, 0, 0, 0];
        "non-ASCII character"
    )]
    fn write_string_to_8_char_buffer_test(
        input: String,
        output: Result<(), AsciiConversionError>,
        expected: [c_char; 8],
    ) {
        let mut buffer: [c_char; 8] = [0; 8];
        assert_eq!(write_string_to_ascii_buffer(input, &mut buffer), output);
        assert_eq!(buffer, expected);
    }
}
