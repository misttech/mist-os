// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains translation between fuchsia.net.filter data structures and Linux
// iptables structures.

use fidl_fuchsia_net_filter_ext::{
    Domain, InstalledIpRoutine, InstalledNatRoutine, IpHook, Namespace, NamespaceId, NatHook,
    Routine, RoutineId, RoutineType, Rule,
};
use starnix_logging::track_stub;
use starnix_uapi::{
    c_char, c_int, errno, error, errors::Errno, ipt_entry, ipt_replace,
    xt_entry_match__bindgen_ty_1__bindgen_ty_1 as xt_entry_match,
    xt_entry_target__bindgen_ty_1__bindgen_ty_1 as xt_entry_target, xt_error_target,
    xt_standard_target, xt_tcp, xt_udp,
};
use std::collections::HashMap;
use std::ffi::CStr;
use zerocopy::{FromBytes, FromZeros, NoCell};

const TABLE_NAT: &str = "nat";
const CHAIN_PREROUTING: &str = "PREROUTING";
const CHAIN_INPUT: &str = "INPUT";
const CHAIN_FORWARD: &str = "FORWARD";
const CHAIN_OUTPUT: &str = "OUTPUT";
const CHAIN_POSTROUTING: &str = "POSTROUTING";

#[derive(Debug)]
pub struct IpTable {
    /// The original ipt_replace struct from Linux.
    /// TODO(b/307908515): Remove once we can recreate this information from other fields.
    pub ipt_replace: ipt_replace,

    /// `namespace`, `routines` and `rules` make up an IPTable's representation
    /// in fuchsia.net.filter's API, where Namespace stores metadata about the table
    /// like its name, Routine correspond to a chain on the table, and Rule is a rule
    /// on a chain. We can update the table state of the system by dropping the Namespace,
    /// then recreating the Namespace, Routines, and Rules in that order.
    pub namespace: Namespace,
    pub routines: Vec<Routine>,
    pub rules: Vec<Rule>,
}

/// Parse an ipt_replace struct and subsequent ipt_entry structs.
pub fn parse_ipt_replace(bytes: &[u8]) -> Result<IpTable, Errno> {
    let replace_info = ipt_replace::read_from_prefix(&bytes).ok_or_else(|| errno!(EINVAL))?;
    let bytes = &bytes[std::mem::size_of::<ipt_replace>()..];

    if bytes.len() != usize::try_from(replace_info.size).unwrap() {
        return error!(EINVAL);
    }

    let mut bytes_read = 0usize;
    let mut entries: Vec<IptEntry> = vec![];

    while bytes_read < bytes.len() {
        let mut entry = parse_ipt_entry(&bytes[bytes_read..])?;
        entry.byte_offset = bytes_read;
        bytes_read += entry.size;
        entries.push(entry);
    }

    if entries.len() != usize::try_from(replace_info.num_entries).unwrap() {
        return error!(EINVAL);
    }

    let table_name = ascii_slice_to_string(&replace_info.name)?;
    let routines = create_routines(&table_name, &entries)?;

    Ok(IpTable {
        ipt_replace: replace_info,
        namespace: Namespace { id: NamespaceId(table_name), domain: Domain::Ipv4 },
        routines: routines.into_values().collect(),
        // TODO(b/307908515): Create Rules from entries.
        rules: vec![],
    })
}

// Creates a hashmap of Routines, keyed by their byte offsets.
// This is useful for parsing Jump targets, which identify their target by the
// offset in the original byte array.
fn create_routines(
    table_name: &String,
    entries: &Vec<IptEntry>,
) -> Result<HashMap<usize, Routine>, Errno> {
    let mut routines: HashMap<usize, Routine> = HashMap::new();

    for (idx, entry) in entries.iter().enumerate() {
        if let TargetType::XtErrorTarget(chain_name) = &entry.target.target_type {
            if idx == entries.len() - 1 {
                // The last entry is always an XtErrorTarget.
                break;
            }

            if !entry.matches.is_empty() {
                return error!(EINVAL);
            }

            let routine_type = match (table_name.as_str(), chain_name.as_str()) {
                (TABLE_NAT, CHAIN_PREROUTING) => RoutineType::Nat(Some(InstalledNatRoutine {
                    hook: NatHook::Ingress,
                    priority: 0,
                })),
                (TABLE_NAT, CHAIN_INPUT) => RoutineType::Nat(Some(InstalledNatRoutine {
                    hook: NatHook::LocalIngress,
                    priority: 0,
                })),
                (TABLE_NAT, CHAIN_OUTPUT) => RoutineType::Nat(Some(InstalledNatRoutine {
                    hook: NatHook::LocalEgress,
                    priority: 0,
                })),
                (TABLE_NAT, CHAIN_POSTROUTING) => RoutineType::Nat(Some(InstalledNatRoutine {
                    hook: NatHook::Egress,
                    priority: 0,
                })),
                (TABLE_NAT, _) => RoutineType::Nat(None),
                (_, CHAIN_PREROUTING) => {
                    RoutineType::Ip(Some(InstalledIpRoutine { hook: IpHook::Ingress, priority: 0 }))
                }
                (_, CHAIN_INPUT) => RoutineType::Ip(Some(InstalledIpRoutine {
                    hook: IpHook::LocalIngress,
                    priority: 0,
                })),
                (_, CHAIN_FORWARD) => RoutineType::Ip(Some(InstalledIpRoutine {
                    hook: IpHook::Forwarding,
                    priority: 0,
                })),
                (_, CHAIN_OUTPUT) => RoutineType::Ip(Some(InstalledIpRoutine {
                    hook: IpHook::LocalEgress,
                    priority: 0,
                })),
                (_, CHAIN_POSTROUTING) => {
                    RoutineType::Ip(Some(InstalledIpRoutine { hook: IpHook::Egress, priority: 0 }))
                }
                (_, _) => RoutineType::Ip(None),
            };

            routines.insert(
                entry.byte_offset,
                Routine {
                    id: RoutineId {
                        namespace: NamespaceId(table_name.clone()),
                        name: chain_name.clone(),
                    },
                    routine_type: routine_type,
                },
            );
        }
    }

    Ok(routines)
}

#[derive(Debug)]
pub enum TargetType {
    Unknown,
    XtStandardTarget(c_int),
    XtErrorTarget(String),
}

#[derive(Debug)]
pub struct XtEntryTarget {
    pub size: usize,
    pub xt_entry_target: xt_entry_target,
    pub target_type: TargetType,
}

#[derive(Debug)]
pub enum MatchType {
    Unknown,
    Tcp(xt_tcp),
    Udp(xt_udp),
}

#[derive(Debug)]
pub struct XtEntryMatch {
    pub size: usize,
    pub xt_entry_match: xt_entry_match,
    pub match_type: MatchType,
}

#[derive(Debug)]
pub struct IptEntry {
    pub size: usize,
    pub ipt_entry: ipt_entry,
    pub matches: Vec<XtEntryMatch>,
    pub target: XtEntryTarget,
    pub byte_offset: usize,
}

/// Parse an ipt_entry struct and subsequent xt_entry_match and xt_entry_target structs.
fn parse_ipt_entry(bytes: &[u8]) -> Result<IptEntry, Errno> {
    let entry_info = ipt_entry::read_from_prefix(&bytes).ok_or_else(|| errno!(EINVAL))?;
    let mut bytes_read = std::mem::size_of::<ipt_entry>();

    let target_offset = usize::from(entry_info.target_offset);
    if target_offset < bytes_read {
        return error!(EINVAL);
    }
    let next_offset = usize::from(entry_info.next_offset);
    if next_offset < bytes_read {
        return error!(EINVAL);
    }

    let mut matches: Vec<XtEntryMatch> = vec![];

    // ipt_entry is followed by 0 or more xt_entry_match's.
    while bytes_read < target_offset {
        if bytes_read + std::mem::size_of::<xt_entry_match>() > target_offset {
            return error!(EINVAL);
        }

        let entry_match = parse_xt_entry_match(&bytes[bytes_read..])?;
        bytes_read += entry_match.size;
        matches.push(entry_match);
    }

    if bytes_read != target_offset {
        return error!(EINVAL);
    }

    let target = parse_xt_entry_target(&bytes[bytes_read..])?;
    bytes_read += target.size;

    if bytes_read != next_offset {
        return error!(EINVAL);
    }

    Ok(IptEntry { size: bytes_read, ipt_entry: entry_info, matches, target, byte_offset: 0 })
}

// Parses an xt_entry_match struct and a specified matcher struct.
fn parse_xt_entry_match(bytes: &[u8]) -> Result<XtEntryMatch, Errno> {
    let match_info = xt_entry_match::read_from_prefix(&bytes).ok_or_else(|| errno!(EINVAL))?;
    let bytes_read = std::mem::size_of::<xt_entry_match>();

    let match_size = usize::from(match_info.match_size);
    if match_size < bytes_read {
        return error!(EINVAL);
    }

    let match_type = match ascii_slice_to_string(&match_info.name)?.as_str() {
        "tcp" => {
            let tcp =
                xt_tcp::read_from_prefix(&bytes[bytes_read..]).ok_or_else(|| errno!(EINVAL))?;
            MatchType::Tcp(tcp)
        }

        "udp" => {
            let udp =
                xt_udp::read_from_prefix(&bytes[bytes_read..]).ok_or_else(|| errno!(EINVAL))?;
            MatchType::Udp(udp)
        }

        _ => {
            track_stub!(TODO("https://fxbug.dev/322875624"), "match extension not supported");
            MatchType::Unknown
        }
    };

    // Returning `match_size` to account for unsupported target extensions.
    Ok(XtEntryMatch { size: match_size, xt_entry_match: match_info, match_type })
}

#[derive(Debug, FromBytes, FromZeros, NoCell)]
struct ErrorTarget {
    pub error_name: [c_char; 30usize],
}

fn parse_xt_entry_target(bytes: &[u8]) -> Result<XtEntryTarget, Errno> {
    let target_info = xt_entry_target::read_from_prefix(&bytes).ok_or_else(|| errno!(EINVAL))?;
    let bytes_read = std::mem::size_of::<xt_entry_target>();

    let target_size = usize::from(target_info.target_size);
    if target_size < bytes_read {
        return error!(EINVAL);
    }

    let target_type = match ascii_slice_to_string(&target_info.name)?.as_str() {
        "" => {
            if target_size != std::mem::size_of::<xt_standard_target>() {
                return error!(EINVAL);
            }
            let verdict =
                c_int::read_from_prefix(&bytes[bytes_read..]).ok_or_else(|| errno!(EINVAL))?;
            TargetType::XtStandardTarget(verdict)
        }

        "ERROR" => {
            if target_size != std::mem::size_of::<xt_error_target>() {
                return error!(EINVAL);
            }
            let error_target = ErrorTarget::read_from_prefix(&bytes[bytes_read..])
                .ok_or_else(|| errno!(EINVAL))?;
            let error_name = ascii_slice_to_string(&error_target.error_name)?;
            TargetType::XtErrorTarget(error_name)
        }

        _ => {
            track_stub!(TODO("https://fxbug.dev/322875546"), "target extension not supported");
            TargetType::Unknown
        }
    };

    // Returning `target_size` to account for unsupported target extensions, and padding in standard/error targets.
    Ok(XtEntryTarget { size: target_size, xt_entry_target: target_info, target_type })
}

// On x86_64, `c_chars` are `i8`. Checks that they are non-negative and convert them to `u8`.
#[cfg(target_arch = "x86_64")]
fn ascii_slice_to_u8_vec(chars: &[c_char]) -> Result<Vec<u8>, Errno> {
    let mut vec = Vec::<u8>::with_capacity(chars.len());
    for &c in chars {
        // This errors if the char is negative.
        vec.push(u8::try_from(c).map_err(|_| errno!(EINVAL))?);
    }
    Ok(vec)
}

// On aarch64 and riscv64, `c_chars` are already `u8`.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn ascii_slice_to_u8_vec(chars: &[c_char]) -> Result<Vec<u8>, Errno> {
    Ok(chars.to_owned())
}

fn ascii_slice_to_string(chars: &[c_char]) -> Result<String, Errno> {
    let bytes = ascii_slice_to_u8_vec(chars)?;
    let c_str = CStr::from_bytes_until_nul(&bytes).map_err(|_| errno!(EINVAL))?;
    c_str.to_str().map_err(|_| errno!(EINVAL)).map(|s| s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{ascii_slice_to_string, parse_ipt_replace};
    use starnix_uapi::{
        c_char, c_int, ipt_entry, ipt_replace,
        xt_entry_target__bindgen_ty_1__bindgen_ty_1 as xt_entry_target,
    };
    use zerocopy::{AsBytes, NoCell};

    #[repr(C)]
    #[derive(AsBytes, Default, NoCell)]
    struct ErrornameWithPadding {
        pub errorname: [c_char; 30usize],
        pub __bindgen_padding_0: [u8; 2usize],
    }

    #[repr(C)]
    #[derive(AsBytes, Default, NoCell)]
    struct VerdictWithPadding {
        pub verdict: c_int,
        pub __bindgen_padding_0: [u8; 4usize],
    }

    #[::fuchsia::test]
    fn parse_table_with_one_empty_chain() {
        let mut bytes: Vec<u8> = vec![];
        bytes.extend_from_slice(
            ipt_replace {
                // "filter"
                name: [
                    102, 105, 108, 116, 101, 114, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                ],
                num_entries: 3,
                size: 504,
                ..Default::default()
            }
            .as_bytes(),
        );

        // First entry is the name of the chain.
        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 176, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target {
                target_size: 64,
                // "ERROR"
                name: [
                    69, 82, 82, 79, 82, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0,
                ],
                revision: 0,
            }
            .as_bytes(),
        );
        bytes.extend_from_slice(
            ErrornameWithPadding {
                // "mychain"
                errorname: [
                    109, 121, 99, 104, 97, 105, 110, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0,
                ],
                ..Default::default()
            }
            .as_bytes(),
        );

        // Second entry is the policy of the chain.
        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 152, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: 40, ..Default::default() }.as_bytes(),
        );
        bytes
            .extend_from_slice(VerdictWithPadding { verdict: -5, ..Default::default() }.as_bytes());

        // Third entry is an error target denoting the end of input.
        bytes.extend_from_slice(
            ipt_entry { target_offset: 112, next_offset: 176, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target {
                target_size: 64,
                // "ERROR"
                name: [
                    69, 82, 82, 79, 82, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0,
                ],
                revision: 0,
            }
            .as_bytes(),
        );
        bytes.extend_from_slice(
            ErrornameWithPadding {
                // "ERROR"
                errorname: [
                    69, 82, 82, 79, 82, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0,
                ],
                ..Default::default()
            }
            .as_bytes(),
        );

        let table = parse_ipt_replace(&bytes).unwrap();

        assert_eq!(table.namespace.id.0, "filter");
        assert_eq!(table.routines.len(), 1);
        let routine_id = &table.routines.first().as_ref().unwrap().id;
        assert_eq!(routine_id.name, "mychain");
        assert_eq!(routine_id.namespace.0, "filter");
    }

    #[::fuchsia::test]
    fn ascii_slice_to_string_test() {
        let bytes: [c_char; 8] = [69, 82, 82, 79, 82, 0, 0, 0];
        assert_eq!("ERROR", ascii_slice_to_string(&bytes).unwrap());

        let bytes: [c_char; 8] = [0; 8];
        assert_eq!("", ascii_slice_to_string(&bytes).unwrap());
    }
}
