// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains translation between fuchsia.net.filter data structures and Linux
// iptables structures.

use starnix_logging::log_warn;
use starnix_uapi::iptables_flags::IptIpInverseFlags;
use starnix_uapi::{
    c_char, c_int, c_uint, ipt_entry, ipt_replace,
    xt_entry_match__bindgen_ty_1__bindgen_ty_1 as xt_entry_match,
    xt_entry_target__bindgen_ty_1__bindgen_ty_1 as xt_entry_target, xt_tcp, xt_udp, IPPROTO_IP,
    IPPROTO_TCP, IPPROTO_UDP, IPT_RETURN, NF_ACCEPT, NF_DROP, NF_QUEUE,
};
use std::any::type_name;
use std::collections::HashMap;
use std::ffi::{CStr, CString, NulError};
use std::mem::size_of;
use std::str::Utf8Error;
use thiserror::Error;
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};
use {fidl_fuchsia_net as fnet, fidl_fuchsia_net_filter_ext as fnet_filter_ext};

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
    AsciiConversion(#[from] AsciiConversionError),
    #[error("FIDL conversion error: {0}")]
    FidlConversion(#[from] fnet_filter_ext::FidlConversionError),
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
    #[error("found chain {chain_name} with no policy entry")]
    ChainHasNoPolicy { chain_name: String },
    #[error("found rule specification before first chain definition")]
    RuleBeforeFirstChain,
    #[error("address subnet mask {mask:#b} has non-prefix bits")]
    SubnetMaskHasNonPrefixBits { mask: u32 },
    #[error("invalid inverse flags {flags:#x} found in rule specification")]
    InvalidInverseFlags { flags: u8 },
    #[error("invalid standard target verdict {verdict}")]
    InvalidVerdict { verdict: i32 },
    #[error("invalid jump target {jump_target}")]
    InvalidJumpTarget { jump_target: usize },
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

#[derive(Clone, Debug)]
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

    // Translated from `xt_standard_target`, which contains a numerical verdict.
    //
    // A 0 or positive verdict is a JUMP to another chain or rule, and a negative verdict
    // is one of the builtin targets like ACCEPT, DROP or RETURN.
    Standard(c_int),

    // Translated from `xt_error_target`, which contains a string.
    //
    // This misleading variant name does not indicate an error in parsing/translation, but rather
    // the start of a chain or the end of input. The inner string is either the name of a chain
    // that the following rule-specifications belong to, or "ERROR" if it is the last entry in the
    // list of entries. Note that "ERROR" does not necessarily indicate the last entry, as a chain
    // can be named "ERROR".
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
    pub namespace: fnet_filter_ext::Namespace,
    pub routine_map: HashMap<usize, fnet_filter_ext::Routine>,
    pub rule_specs: Vec<RuleSpec>,
}

impl IpTable {
    pub fn from_ipt_replace(bytes: Vec<u8>) -> Result<Self, IpTableParseError> {
        let mut parser = IptReplaceParser::new(bytes)?;
        let table_name = parser.replace_info.name.clone();

        let mut entries: Vec<Entry> = vec![];

        // Pass 1: Parse all bytes into Linux structs as `Entry`s.
        while !parser.finished() {
            entries.push(parser.parse_entry()?);
        }

        if entries.len() != parser.replace_info.num_entries {
            return Err(IpTableParseError::NumEntriesMismatch {
                specified: parser.replace_info.num_entries,
                found: entries.len(),
            });
        }

        // Pass 2: Translate chain-definition entries into `Routine`s, and wrap rule-specification
        // entries in `RuleSpecs::Untranslated`.
        let (routine_map, untranslated_rules) = create_routine_map_and_rules(&table_name, entries)?;

        // Pass 3: Translate `RuleSpecs` into `Rule`s.
        let rule_specs: Vec<_> = untranslated_rules
            .into_iter()
            .map(|untranslated| RuleSpec::try_translate(untranslated, &routine_map))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(IpTable {
            parser,
            namespace: fnet_filter_ext::Namespace {
                id: fnet_filter_ext::NamespaceId(table_name),
                domain: fnet_filter_ext::Domain::Ipv4,
            },
            routine_map,
            rule_specs,
        })
    }

    pub fn into_changes(self) -> impl Iterator<Item = fnet_filter_ext::Change> {
        let namespace_changes = [
            // Firstly, remove the existing table, along with all of its routines and rules.
            // We will call Commit with idempotent=true so that this would succeed even if
            // the table did not exist prior to this change.
            fnet_filter_ext::Change::Remove(fnet_filter_ext::ResourceId::Namespace(
                self.namespace.id.clone(),
            )),
            // Recreate the table.
            fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Namespace(self.namespace)),
        ]
        .into_iter();

        let routine_changes = self
            .routine_map
            .into_values()
            .map(fnet_filter_ext::Resource::Routine)
            .map(fnet_filter_ext::Change::Create);

        let rule_changes = self
            .rule_specs
            .into_iter()
            .filter(|spec| matches!(spec, RuleSpec::Translated(_)))
            .filter_map(|spec| match spec {
                RuleSpec::Unsupported(_) => None,
                RuleSpec::Translated(rule) => {
                    Some(fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Rule(rule)))
                }
            });

        namespace_changes.chain(routine_changes).chain(rule_changes)
    }
}

fn get_routine_type(table_name: &str, chain_name: &str) -> fnet_filter_ext::RoutineType {
    match (table_name, chain_name) {
        (TABLE_NAT, CHAIN_PREROUTING) => {
            fnet_filter_ext::RoutineType::Nat(Some(fnet_filter_ext::InstalledNatRoutine {
                hook: fnet_filter_ext::NatHook::Ingress,
                priority: 0,
            }))
        }
        (TABLE_NAT, CHAIN_INPUT) => {
            fnet_filter_ext::RoutineType::Nat(Some(fnet_filter_ext::InstalledNatRoutine {
                hook: fnet_filter_ext::NatHook::LocalIngress,
                priority: 0,
            }))
        }
        (TABLE_NAT, CHAIN_OUTPUT) => {
            fnet_filter_ext::RoutineType::Nat(Some(fnet_filter_ext::InstalledNatRoutine {
                hook: fnet_filter_ext::NatHook::LocalEgress,
                priority: 0,
            }))
        }
        (TABLE_NAT, CHAIN_POSTROUTING) => {
            fnet_filter_ext::RoutineType::Nat(Some(fnet_filter_ext::InstalledNatRoutine {
                hook: fnet_filter_ext::NatHook::Egress,
                priority: 0,
            }))
        }
        (TABLE_NAT, _) => fnet_filter_ext::RoutineType::Nat(None),
        (_, CHAIN_PREROUTING) => {
            fnet_filter_ext::RoutineType::Ip(Some(fnet_filter_ext::InstalledIpRoutine {
                hook: fnet_filter_ext::IpHook::Ingress,
                priority: 0,
            }))
        }
        (_, CHAIN_INPUT) => {
            fnet_filter_ext::RoutineType::Ip(Some(fnet_filter_ext::InstalledIpRoutine {
                hook: fnet_filter_ext::IpHook::LocalIngress,
                priority: 0,
            }))
        }
        (_, CHAIN_FORWARD) => {
            fnet_filter_ext::RoutineType::Ip(Some(fnet_filter_ext::InstalledIpRoutine {
                hook: fnet_filter_ext::IpHook::Forwarding,
                priority: 0,
            }))
        }
        (_, CHAIN_OUTPUT) => {
            fnet_filter_ext::RoutineType::Ip(Some(fnet_filter_ext::InstalledIpRoutine {
                hook: fnet_filter_ext::IpHook::LocalEgress,
                priority: 0,
            }))
        }
        (_, CHAIN_POSTROUTING) => {
            fnet_filter_ext::RoutineType::Ip(Some(fnet_filter_ext::InstalledIpRoutine {
                hook: fnet_filter_ext::IpHook::Egress,
                priority: 0,
            }))
        }
        (_, _) => fnet_filter_ext::RoutineType::Ip(None),
    }
}

#[derive(Debug)]
pub struct UntranslatedRule {
    pub routine_id: fnet_filter_ext::RoutineId,
    pub entry: Entry,
}

#[derive(Debug)]
pub enum RuleSpec {
    Unsupported(UntranslatedRule),
    Translated(fnet_filter_ext::Rule),
}

impl RuleSpec {
    fn try_translate(
        untranslated: UntranslatedRule,
        routine_map: &HashMap<usize, fnet_filter_ext::Routine>,
    ) -> Result<Self, IpTableParseError> {
        let entry = &untranslated.entry;

        let Some(matchers) = entry.get_rule_matchers()? else {
            return Ok(Self::Unsupported(untranslated));
        };

        let Some(action) = entry.get_rule_action(routine_map)? else {
            return Ok(Self::Unsupported(untranslated));
        };

        Ok(Self::Translated(fnet_filter_ext::Rule {
            id: fnet_filter_ext::RuleId {
                routine: untranslated.routine_id,
                index: untranslated.entry.byte_pos as u32,
            },
            matchers,
            action,
        }))
    }
}

impl Entry {
    // Creates a `fnet_filter_ext::Matchers` object and populate it with IP matchers in `ipt_ip`,
    // and extension matchers like `xt_tcp`.
    // Returns None if an unsupported matcher is found.
    fn get_rule_matchers(&self) -> Result<Option<fnet_filter_ext::Matchers>, IpTableParseError> {
        let mut matchers = fnet_filter_ext::Matchers::default();

        if self.populate_matchers_with_ipt_ip(&mut matchers)?.is_none() {
            return Ok(None);
        }
        if self.populate_matchers_with_match_extensions(&mut matchers)?.is_none() {
            return Ok(None);
        }

        Ok(Some(matchers))
    }

    fn get_rule_action(
        &self,
        routine_map: &HashMap<usize, fnet_filter_ext::Routine>,
    ) -> Result<Option<fnet_filter_ext::Action>, IpTableParseError> {
        match self.target {
            Target::Unknown { name: _, bytes: _ } => Ok(None),

            // Error targets should already be translated into `Routine`s.
            Target::Error(_) => {
                unreachable!()
            }

            Target::Standard(verdict) => Self::translate_standard_target(verdict, routine_map),
        }
    }

    fn populate_matchers_with_ipt_ip(
        &self,
        matchers: &mut fnet_filter_ext::Matchers,
    ) -> Result<Option<()>, IpTableParseError> {
        let inv_flags = IptIpInverseFlags::from_bits(self.ipt_entry.ip.invflags.into())
            .ok_or_else(|| IpTableParseError::InvalidInverseFlags {
                flags: self.ipt_entry.ip.invflags,
            })?;
        let ip = &self.ipt_entry.ip;

        if ip.flags != 0 {
            log_warn!("IpTables: ignored rule-specification with IP flags {}", ip.flags);
            return Ok(None);
        }

        if ip.smsk.s_addr != 0 {
            matchers.src_addr = Some(fnet_filter_ext::AddressMatcher {
                matcher: Self::new_address_matcher(ip.src.s_addr, ip.smsk.s_addr)?,
                invert: inv_flags.contains(IptIpInverseFlags::SourceIpAddress),
            });
        }

        if ip.dmsk.s_addr != 0 {
            matchers.dst_addr = Some(fnet_filter_ext::AddressMatcher {
                matcher: Self::new_address_matcher(ip.dst.s_addr, ip.dmsk.s_addr)?,
                invert: inv_flags.contains(IptIpInverseFlags::DestinationIpAddress),
            });
        }

        if ip.iniface_mask != [0u8; 16] {
            if inv_flags.contains(IptIpInverseFlags::InputInterface) {
                log_warn!("IpTables: ignored rule-specification with inversed input interface");
                return Ok(None);
            }
            matchers.in_interface = Some(fnet_filter_ext::InterfaceMatcher::Name(
                ascii_to_string(&ip.iniface).map_err(IpTableParseError::AsciiConversion)?,
            ))
        }

        if ip.outiface_mask != [0u8; 16] {
            if inv_flags.contains(IptIpInverseFlags::OutputInterface) {
                log_warn!("IpTables: ignored rule-specification with inversed output interface");
                return Ok(None);
            }

            matchers.out_interface = Some(fnet_filter_ext::InterfaceMatcher::Name(
                ascii_to_string(&ip.outiface).map_err(IpTableParseError::AsciiConversion)?,
            ))
        }

        match self.ipt_entry.ip.proto {
            // matches both TCP and UDP, which is true by default.
            protocol if u32::from(protocol) == IPPROTO_IP => {}

            protocol if u32::from(protocol) == IPPROTO_TCP => {
                matchers.transport_protocol =
                    Some(fnet_filter_ext::TransportProtocolMatcher::Tcp {
                        // These fields are set later by `xt_tcp` match extension, if present.
                        src_port: None,
                        dst_port: None,
                    });
            }

            protocol if u32::from(protocol) == IPPROTO_UDP => {
                matchers.transport_protocol =
                    Some(fnet_filter_ext::TransportProtocolMatcher::Udp {
                        // These fields are set later by `xt_udp` match extension, if present.
                        src_port: None,
                        dst_port: None,
                    });
            }

            protocol => {
                log_warn!("IpTables: ignored rule-specification with protocol {protocol}");
                return Ok(None);
            }
        };

        Ok(Some(()))
    }

    fn populate_matchers_with_match_extensions(
        &self,
        _matchers: &mut fnet_filter_ext::Matchers,
    ) -> Result<Option<()>, IpTableParseError> {
        if !self.matchers.is_empty() {
            log_warn!("IpTables: ignored rule-specification with match extensions");
            return Ok(None);
        }

        Ok(Some(()))
    }

    fn new_address_matcher(
        ip_addr: u32,
        subnet_mask: u32,
    ) -> Result<fnet_filter_ext::AddressMatcherType, IpTableParseError> {
        let subnet = fnet::Subnet {
            addr: fnet::IpAddress::Ipv4(fnet::Ipv4Address {
                addr: u32::from_be(ip_addr).to_be_bytes(),
            }),
            prefix_len: Self::mask_to_prefix_len(subnet_mask)?,
        };

        let subnet =
            fnet_filter_ext::Subnet::try_from(subnet).map_err(IpTableParseError::FidlConversion)?;

        Ok(fnet_filter_ext::AddressMatcherType::Subnet(subnet))
    }

    // Assumes mask is big endian.
    fn mask_to_prefix_len(mask: u32) -> Result<u8, IpTableParseError> {
        let mask = u32::from_be(mask);

        // Check that all 1's in the mask are before all 0's.
        // To do this, we can simply find if its 2-complement is a power of 2.
        if !mask.wrapping_neg().is_power_of_two() {
            return Err(IpTableParseError::SubnetMaskHasNonPrefixBits { mask });
        }

        // Impossible to have more 1's in a `u32` than 255.
        Ok(mask.count_ones() as u8)
    }

    fn translate_standard_target(
        verdict: i32,
        routine_map: &HashMap<usize, fnet_filter_ext::Routine>,
    ) -> Result<Option<fnet_filter_ext::Action>, IpTableParseError> {
        match verdict {
            // A 0 or positive verdict is a JUMP to another chain or rule, but jumping to another
            // rule is not supported by fuchsia.net.filter.
            verdict if verdict >= 0 => {
                let jump_target = usize::try_from(verdict).expect("positive i32 fits into usize");
                if let Some(routine) = routine_map.get(&jump_target) {
                    Ok(Some(fnet_filter_ext::Action::Jump(routine.id.name.to_owned())))
                } else {
                    Err(IpTableParseError::InvalidJumpTarget { jump_target })
                }
            }

            // A negative verdict is one of the builtin targets.
            // Translate them the same way as the iptables tool.
            verdict if verdict == -(NF_DROP as i32) - 1 => Ok(Some(fnet_filter_ext::Action::Drop)),

            verdict if verdict == -(NF_ACCEPT as i32) - 1 => {
                Ok(Some(fnet_filter_ext::Action::Accept))
            }

            verdict if verdict == -(NF_QUEUE as i32) - 1 => {
                log_warn!("IpTables: ignored unsupported QUEUE target");
                Ok(None)
            }

            verdict if verdict == IPT_RETURN => Ok(Some(fnet_filter_ext::Action::Return)),

            verdict => Err(IpTableParseError::InvalidVerdict { verdict }),
        }
    }
}

// Chain-definitions are returned as `Routine`s, stored in a HashMap keyed by the byte position of
// the chain's first rule/policy (there must be at least one).
// Rules are returned in a Vec as `UntranslatedRule`.
fn create_routine_map_and_rules(
    table_name: &String,
    mut entries: Vec<Entry>,
) -> Result<(HashMap<usize, fnet_filter_ext::Routine>, Vec<UntranslatedRule>), IpTableParseError> {
    // There must be at least 1 entry and the last entry must be an error target named "ERROR".
    let last_entry = entries.last().ok_or_else(|| IpTableParseError::NoTrailingErrorTarget)?;
    if !last_entry.matchers.is_empty() {
        return Err(IpTableParseError::ErrorEntryHasMatchers);
    }
    if let Target::Error(chain_name) = &last_entry.target {
        if chain_name.as_str() != "ERROR" {
            return Err(IpTableParseError::NoTrailingErrorTarget);
        }
    } else {
        return Err(IpTableParseError::NoTrailingErrorTarget);
    }
    entries.truncate(entries.len() - 1);

    let mut routine_map: HashMap<usize, fnet_filter_ext::Routine> = HashMap::new();
    let mut rules: Vec<UntranslatedRule> = Vec::new();

    // A new Routine is first added as a Pending routine, and then inserted into `routine_map`
    // when its first rule or policy is processed. This implementation has 2 advantages:
    //
    // 1. JUMP targets reference the byte position of the first entry after a chain definition,
    //    so we can insert the Routine with the correct byte position.
    // 2. We can catch the translation error where a chain is defined without a policy.
    enum RoutineState {
        // No chain definition has been read.
        None,
        // Routine is not inserted into `routine_map`.
        Pending(fnet_filter_ext::Routine),
        // Routine is inserted into `routine_map`.
        Inserted(fnet_filter_ext::RoutineId),
    }
    let mut current_routine = RoutineState::None;

    for entry in entries.into_iter() {
        if let Target::Error(chain_name) = &entry.target {
            if !entry.matchers.is_empty() {
                return Err(IpTableParseError::ErrorEntryHasMatchers);
            }
            if let RoutineState::Pending(routine) = current_routine {
                return Err(IpTableParseError::ChainHasNoPolicy { chain_name: routine.id.name });
            }

            let routine_id = fnet_filter_ext::RoutineId {
                namespace: fnet_filter_ext::NamespaceId(table_name.clone()),
                name: chain_name.clone(),
            };
            let routine_type = get_routine_type(table_name.as_str(), chain_name.as_str());
            current_routine = RoutineState::Pending(fnet_filter_ext::Routine {
                id: routine_id,
                routine_type: routine_type,
            });
        } else {
            if let RoutineState::None = current_routine {
                return Err(IpTableParseError::RuleBeforeFirstChain);
            }
            if let RoutineState::Pending(routine) = current_routine {
                let routine_id = routine.id.clone();
                routine_map.insert(entry.byte_pos, routine);
                current_routine = RoutineState::Inserted(routine_id);
            }

            // At this stage, current_routine must be the `Inserted` variant.
            let RoutineState::Inserted(ref routine_id) = current_routine else { unreachable!() };
            rules.push(UntranslatedRule { routine_id: routine_id.clone(), entry });
        }
    }

    if let RoutineState::Pending(routine) = current_routine {
        return Err(IpTableParseError::ChainHasNoPolicy { chain_name: routine.id.name });
    }

    Ok((routine_map, rules))
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
    use assert_matches::assert_matches;
    use fidl_fuchsia_net_filter_ext as fnet_filter_ext;
    use net_declare::fidl_subnet;
    use starnix_uapi::{
        c_char, in_addr, ipt_entry, ipt_ip, ipt_replace,
        xt_entry_match__bindgen_ty_1__bindgen_ty_1 as xt_entry_match,
        xt_entry_target__bindgen_ty_1__bindgen_ty_1 as xt_entry_target, xt_tcp, xt_udp,
    };
    use test_case::test_case;

    fn string_to_16_chars(string: &str) -> [c_char; 16] {
        let mut buffer = [0; 16];
        write_string_to_ascii_buffer(String::from(string), &mut buffer).unwrap();
        buffer
    }

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

    fn extend_with_standard_verdict(bytes: &mut Vec<u8>, verdict: i32) {
        bytes.extend_from_slice(
            xt_entry_target { target_size: 40, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(VerdictWithPadding { verdict, ..Default::default() }.as_bytes());
    }

    fn extend_with_standard_target_entry(bytes: &mut Vec<u8>, verdict: i32) {
        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 152, ..Default::default() }.as_bytes(),
        );
        extend_with_standard_verdict(bytes, verdict);
    }

    fn extend_with_error_target_entry(bytes: &mut Vec<u8>, error_name: &str) {
        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 176, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: 64, name: string_to_29_chars("ERROR"), revision: 0 }
                .as_bytes(),
        );
        bytes.extend_from_slice(
            ErrorNameWithPadding {
                errorname: string_to_30_chars(error_name),
                ..Default::default()
            }
            .as_bytes(),
        );
    }

    fn table_with_ip_matchers() -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![];

        bytes.extend_from_slice(
            ipt_replace {
                name: string_to_32_chars("filter"),
                num_entries: 5,
                size: 808,
                ..Default::default()
            }
            .as_bytes(),
        );

        // Entry 1: start of the chain.
        extend_with_error_target_entry(&mut bytes, "mychain");

        // Entry 2: accept TCP packets from 10.0.0.1.
        bytes.extend_from_slice(
            ipt_entry {
                ip: ipt_ip {
                    src: in_addr { s_addr: 16777226 },
                    smsk: in_addr { s_addr: 4294967295 },
                    proto: 6,
                    ..Default::default()
                },
                target_offset: 112,
                next_offset: 152,
                ..Default::default()
            }
            .as_bytes(),
        );
        extend_with_standard_verdict(&mut bytes, -2);

        // Entry 3: drop all packets going to en0 interface.
        bytes.extend_from_slice(
            ipt_entry {
                ip: ipt_ip {
                    iniface: string_to_16_chars("en0"),
                    iniface_mask: [255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                    ..Default::default()
                },
                target_offset: 112,
                next_offset: 152,
                ..Default::default()
            }
            .as_bytes(),
        );
        extend_with_standard_verdict(&mut bytes, -1);

        // Entry 4: "policy" of the chain.
        extend_with_standard_target_entry(&mut bytes, -5);

        // Entry 5: end of input.
        extend_with_error_target_entry(&mut bytes, "ERROR");

        bytes
    }

    #[fuchsia::test]
    fn parse_ip_matchers_test() {
        let table = IpTable::from_ipt_replace(table_with_ip_matchers()).unwrap();
        assert_eq!(table.namespace.id.0, "filter");

        let routines: Vec<_> = table.routine_map.into_values().collect();
        assert_eq!(routines.len(), 1);

        let routine_id = &routines.first().unwrap().id;
        assert_eq!(routine_id.name, "mychain");
        assert_eq!(routine_id.namespace.0, "filter");

        let mut rules = table.rule_specs.iter();
        let RuleSpec::Translated(rule1) = rules.next().expect("rule 1 exists") else {
            panic!("rule 1 should be translated");
        };
        let expected_rule1 = fnet_filter_ext::Rule {
            id: fnet_filter_ext::RuleId { index: 176, routine: routine_id.clone() },
            matchers: fnet_filter_ext::Matchers {
                src_addr: Some(fnet_filter_ext::AddressMatcher {
                    matcher: fnet_filter_ext::AddressMatcherType::Subnet(
                        fnet_filter_ext::Subnet::try_from(fidl_subnet!("10.0.0.1/32")).unwrap(),
                    ),
                    invert: false,
                }),
                transport_protocol: Some(fnet_filter_ext::TransportProtocolMatcher::Tcp {
                    dst_port: None,
                    src_port: None,
                }),
                ..Default::default()
            },
            action: fnet_filter_ext::Action::Accept,
        };
        assert_eq!(rule1, &expected_rule1);

        let RuleSpec::Translated(rule2) = rules.next().expect("rule 2 exists") else {
            panic!("rule 2 should be translated");
        };
        let expected_rule2 = fnet_filter_ext::Rule {
            id: fnet_filter_ext::RuleId { index: 328, routine: routine_id.clone() },
            matchers: fnet_filter_ext::Matchers {
                in_interface: Some(fnet_filter_ext::InterfaceMatcher::Name("en0".to_string())),
                ..Default::default()
            },
            action: fnet_filter_ext::Action::Drop,
        };
        assert_eq!(rule2, &expected_rule2);

        let RuleSpec::Translated(rule3) = rules.next().expect("rule 3 exists") else {
            panic!("rule 3 should be translated");
        };
        let expected_rule3 = fnet_filter_ext::Rule {
            id: fnet_filter_ext::RuleId { index: 480, routine: routine_id.clone() },
            matchers: fnet_filter_ext::Matchers::default(),
            action: fnet_filter_ext::Action::Return,
        };
        assert_eq!(rule3, &expected_rule3);

        assert!(rules.next().is_none());
    }

    fn table_with_match_extensions() -> Vec<u8> {
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
        extend_with_error_target_entry(&mut bytes, "mychain");

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
        extend_with_standard_verdict(&mut bytes, -5);

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
        extend_with_standard_verdict(&mut bytes, -5);

        // Entry 4: "policy" of the chain.
        extend_with_standard_target_entry(&mut bytes, -5);

        // Entry 5: end of input.
        extend_with_error_target_entry(&mut bytes, "ERROR");

        bytes
    }

    #[fuchsia::test]
    fn parse_match_extensions_test() {
        let table = IpTable::from_ipt_replace(table_with_match_extensions()).unwrap();
        assert_eq!(table.namespace.id.0, "filter");

        let routines: Vec<_> = table.routine_map.into_values().collect();
        assert_eq!(routines.len(), 1);

        let routine_id = &routines.first().unwrap().id;
        assert_eq!(routine_id.name, "mychain");
        assert_eq!(routine_id.namespace.0, "filter");

        let mut rules = table.rule_specs.iter();

        assert_matches!(rules.next(), Some(&RuleSpec::Unsupported(_)));
        assert_matches!(rules.next(), Some(&RuleSpec::Unsupported(_)));

        let RuleSpec::Translated(rule3) = rules.next().expect("rule 3 exists") else {
            panic!("rule 3 should be translated");
        };
        let expected_rule3 = fnet_filter_ext::Rule {
            id: fnet_filter_ext::RuleId { index: 576, routine: routine_id.clone() },
            matchers: fnet_filter_ext::Matchers::default(),
            action: fnet_filter_ext::Action::Return,
        };
        assert_eq!(rule3, &expected_rule3);

        assert!(rules.next().is_none());
    }

    fn table_with_jump_target() -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![];

        bytes.extend_from_slice(
            ipt_replace {
                name: string_to_32_chars("filter"),
                num_entries: 6,
                size: 984,
                ..Default::default()
            }
            .as_bytes(),
        );

        // Entry 1: start of the chain1.
        extend_with_error_target_entry(&mut bytes, "chain1");

        // Entry 2: jump to chain2 for all packets.
        extend_with_standard_target_entry(&mut bytes, 656);

        // Entry 3: policy of chain1.
        extend_with_standard_target_entry(&mut bytes, -5);

        // Entry 4: start of chain2.
        extend_with_error_target_entry(&mut bytes, "chain2");

        // Entry 5: policy of chain2.
        extend_with_standard_target_entry(&mut bytes, -5);

        // Entry 6: end of input.
        extend_with_error_target_entry(&mut bytes, "ERROR");

        bytes
    }

    #[fuchsia::test]
    fn parse_jump_target_test() {
        let table = IpTable::from_ipt_replace(table_with_jump_target()).unwrap();

        let mut routines: Vec<_> = table.routine_map.into_values().collect();
        assert_eq!(routines.len(), 2);
        routines.sort_by_key(|routine| routine.id.name.clone());

        let routine1_id = &routines.first().unwrap().id;
        assert_eq!(routine1_id.name, "chain1");
        assert_eq!(routine1_id.namespace.0, "filter");

        let routine2_id = &routines.last().unwrap().id;
        assert_eq!(routine2_id.name, "chain2");
        assert_eq!(routine2_id.namespace.0, "filter");

        let mut rules = table.rule_specs.iter();
        let RuleSpec::Translated(rule1) = rules.next().expect("rule 1 exists") else {
            panic!("rule 1 should be translated");
        };
        let expected_rule1 = fnet_filter_ext::Rule {
            id: fnet_filter_ext::RuleId { index: 176, routine: routine1_id.clone() },
            matchers: fnet_filter_ext::Matchers::default(),
            action: fnet_filter_ext::Action::Jump("chain2".to_string()),
        };
        assert_eq!(rule1, &expected_rule1);

        let RuleSpec::Translated(rule2) = rules.next().expect("rule 2 exists") else {
            panic!("rule 2 should be translated");
        };
        let expected_rule2 = fnet_filter_ext::Rule {
            id: fnet_filter_ext::RuleId { index: 328, routine: routine1_id.clone() },
            matchers: fnet_filter_ext::Matchers::default(),
            action: fnet_filter_ext::Action::Return,
        };
        assert_eq!(rule2, &expected_rule2);

        let RuleSpec::Translated(rule3) = rules.next().expect("rule 3 exists") else {
            panic!("rule 3 should be translated");
        };
        let expected_rule3 = fnet_filter_ext::Rule {
            id: fnet_filter_ext::RuleId { index: 656, routine: routine2_id.clone() },
            matchers: fnet_filter_ext::Matchers::default(),
            action: fnet_filter_ext::Action::Return,
        };
        assert_eq!(rule3, &expected_rule3);
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

    fn table_with_chain_with_no_policy() -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![];

        bytes.extend_from_slice(
            ipt_replace {
                name: string_to_32_chars("filter"),
                num_entries: 2,
                size: 352,
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

        // Entry 2: end of input.
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
    #[test_case(
        table_with_chain_with_no_policy(),
        IpTableParseError::ChainHasNoPolicy {
            chain_name: String::from("mychain"),
        };
        "chain with no policy"
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
