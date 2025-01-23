// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::MemoryAccessorExt;
use crate::task::CurrentTask;
use crate::vfs::socket::iptables_utils::{self, write_string_to_ascii_buffer};
use crate::vfs::socket::{SocketDomain, SocketHandle, SocketType};
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext::sync::Controller;
use fidl_fuchsia_net_filter_ext::{
    Change, CommitError, ControllerCreationError, ControllerId, PushChangesError,
};
use fuchsia_component::client::connect_to_protocol_sync;
use itertools::Itertools;
use starnix_logging::{log_warn, track_stub};
use starnix_types::user_buffer::UserBuffer;
use starnix_uapi::errors::Errno;
use starnix_uapi::iptables_flags::NfIpHooks;
use starnix_uapi::{
    c_char, errno, error, ip6t_entry, ip6t_get_entries, ip6t_getinfo, ipt_entry, ipt_get_entries,
    ipt_getinfo, nf_inet_hooks_NF_INET_NUMHOOKS, xt_counters, xt_counters_info,
    xt_entry_target__bindgen_ty_1__bindgen_ty_1 as xt_entry_target, xt_error_target,
    xt_get_revision, xt_standard_target, IP6T_SO_GET_ENTRIES, IP6T_SO_GET_INFO,
    IP6T_SO_GET_REVISION_MATCH, IP6T_SO_GET_REVISION_TARGET, IPT_SO_GET_ENTRIES, IPT_SO_GET_INFO,
    IPT_SO_GET_REVISION_MATCH, IPT_SO_GET_REVISION_TARGET, IPT_SO_SET_ADD_COUNTERS,
    IPT_SO_SET_REPLACE, SOL_IP, SOL_IPV6, XT_EXTENSION_MAXNAMELEN, XT_FUNCTION_MAXNAMELEN,
    XT_TABLE_MAXNAMELEN,
};
use std::collections::HashMap;
use std::mem::size_of;
use thiserror::Error;
use zerocopy::{FromBytes, IntoBytes};

const NAMESPACE_ID_PREFIX: &str = "starnix";

const TARGET_NAME_LEN: usize = XT_EXTENSION_MAXNAMELEN as usize;
const ERROR_NAME_LEN: usize = XT_FUNCTION_MAXNAMELEN as usize;
const TABLE_NAME_LEN: usize = XT_TABLE_MAXNAMELEN as usize;

const IPT_ENTRY_SIZE: u16 = size_of::<ipt_entry>() as u16;
const IP6T_ENTRY_SIZE: u16 = size_of::<ip6t_entry>() as u16;
const STANDARD_TARGET_SIZE: u16 = size_of::<xt_standard_target>() as u16;
const ERROR_TARGET_SIZE: u16 = size_of::<xt_error_target>() as u16;

// The following arrays denote where built-in chains are defined for each table. This makes it easy
// to calculate `hook_entry` and `underflow` for tables where built-in chains only have a policy and
// no other rules by scaling by the size of a standard entry.
//
// The indices correspond to [PREROUTING, INPUT, FORWARD, OUTPUT, POSTROUTING]. The first built-in
// chain has value 0, second chain has value 1, and so on. Confusingly, built-in chains that do not
// exist on a table are also denoted as 0, but this is how Linux expects these values.
const FILTER_HOOKS: [u32; 5] = [0, 0, 1, 2, 0];
const NAT_HOOKS: [u32; 5] = [0, 1, 0, 2, 3];
const MANGLE_HOOKS: [u32; 5] = [0, 1, 2, 3, 4];
const RAW_HOOKS: [u32; 5] = [0, 0, 0, 1, 0];

type IpTablesName = [c_char; TABLE_NAME_LEN];

/// Stores information about IP packet filter rules. Used to return information for
/// IPT_SO_GET_INFO and IPT_SO_GET_ENTRIES.
#[derive(Debug, Default)]
struct IpTable {
    pub valid_hooks: u32,
    pub hook_entry: [u32; nf_inet_hooks_NF_INET_NUMHOOKS as usize],
    pub underflow: [u32; nf_inet_hooks_NF_INET_NUMHOOKS as usize],
    pub num_entries: u32,
    pub size: u32,
    pub entries: Vec<u8>,
    pub num_counters: u32,
    pub counters: Vec<xt_counters>,
}

impl IpTable {
    fn accept_policy_v4() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            ipt_entry {
                target_offset: IPT_ENTRY_SIZE,
                next_offset: IPT_ENTRY_SIZE + STANDARD_TARGET_SIZE,
                ..Default::default()
            }
            .as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: STANDARD_TARGET_SIZE, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            iptables_utils::VerdictWithPadding {
                verdict: iptables_utils::VERDICT_ACCEPT,
                ..Default::default()
            }
            .as_bytes(),
        );
        bytes
    }

    fn accept_policy_v6() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            ip6t_entry {
                target_offset: IP6T_ENTRY_SIZE,
                next_offset: IP6T_ENTRY_SIZE + STANDARD_TARGET_SIZE,
                ..Default::default()
            }
            .as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: STANDARD_TARGET_SIZE, ..Default::default() }.as_bytes(),
        );
        bytes.extend_from_slice(
            iptables_utils::VerdictWithPadding {
                verdict: iptables_utils::VERDICT_ACCEPT,
                ..Default::default()
            }
            .as_bytes(),
        );
        bytes
    }

    fn end_of_input_v4() -> Vec<u8> {
        let mut target_name = [0; TARGET_NAME_LEN];
        write_string_to_ascii_buffer("ERROR".to_owned(), &mut target_name)
            .expect("convert \"ERROR\" to ASCII");
        let mut errorname = [0; ERROR_NAME_LEN];
        write_string_to_ascii_buffer("ERROR".to_owned(), &mut errorname)
            .expect("convert \"ERROR\" to ASCII");

        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            ipt_entry {
                target_offset: IPT_ENTRY_SIZE,
                next_offset: IPT_ENTRY_SIZE + ERROR_TARGET_SIZE,
                ..Default::default()
            }
            .as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: ERROR_TARGET_SIZE, name: target_name, revision: 0 }
                .as_bytes(),
        );
        bytes.extend_from_slice(
            iptables_utils::ErrorNameWithPadding { errorname, ..Default::default() }.as_bytes(),
        );
        bytes
    }

    fn end_of_input_v6() -> Vec<u8> {
        let mut target_name = [0; TARGET_NAME_LEN];
        write_string_to_ascii_buffer("ERROR".to_owned(), &mut target_name)
            .expect("convert \"ERROR\" to ASCII");
        let mut errorname = [0; ERROR_NAME_LEN];
        write_string_to_ascii_buffer("ERROR".to_owned(), &mut errorname)
            .expect("convert \"ERROR\" to ASCII");

        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            ip6t_entry {
                target_offset: IP6T_ENTRY_SIZE,
                next_offset: IP6T_ENTRY_SIZE + ERROR_TARGET_SIZE,
                ..Default::default()
            }
            .as_bytes(),
        );
        bytes.extend_from_slice(
            xt_entry_target { target_size: ERROR_TARGET_SIZE, name: target_name, revision: 0 }
                .as_bytes(),
        );
        bytes.extend_from_slice(
            iptables_utils::ErrorNameWithPadding { errorname, ..Default::default() }.as_bytes(),
        );
        bytes
    }

    fn default_ipv4_nat_table() -> (IpTablesName, Self) {
        let mut table_name = [0; TABLE_NAME_LEN];
        write_string_to_ascii_buffer("nat".to_owned(), &mut table_name)
            .expect("convert \"nat\" to ASCII");

        let hook_entry = NAT_HOOKS.map(|n| n * u32::from(IPT_ENTRY_SIZE + STANDARD_TARGET_SIZE));
        let mut entries = Vec::new();
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::end_of_input_v4());

        (
            table_name,
            Self {
                valid_hooks: NfIpHooks::NAT.bits(),
                hook_entry,
                underflow: hook_entry,
                num_entries: 5,
                size: entries.len() as u32,
                entries,
                ..Default::default()
            },
        )
    }

    fn default_ipv6_nat_table() -> (IpTablesName, Self) {
        let mut table_name = [0; TABLE_NAME_LEN];
        write_string_to_ascii_buffer("nat".to_owned(), &mut table_name)
            .expect("convert \"nat\" to ASCII");

        let hook_entry = NAT_HOOKS.map(|n| n * u32::from(IP6T_ENTRY_SIZE + STANDARD_TARGET_SIZE));
        let mut entries = Vec::new();
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::end_of_input_v6());

        (
            table_name,
            Self {
                valid_hooks: NfIpHooks::NAT.bits(),
                hook_entry,
                underflow: hook_entry,
                num_entries: 5,
                size: entries.len() as u32,
                entries,
                ..Default::default()
            },
        )
    }

    fn default_ipv4_filter_table() -> (IpTablesName, Self) {
        let mut table_name = [0; TABLE_NAME_LEN];
        write_string_to_ascii_buffer("filter".to_owned(), &mut table_name)
            .expect("convert \"filter\" to ASCII");

        let hook_entry = FILTER_HOOKS.map(|n| n * u32::from(IPT_ENTRY_SIZE + STANDARD_TARGET_SIZE));
        let mut entries = Vec::new();
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::end_of_input_v4());

        (
            table_name,
            Self {
                valid_hooks: NfIpHooks::FILTER.bits(),
                hook_entry,
                underflow: hook_entry,
                num_entries: 4,
                size: entries.len() as u32,
                entries,
                ..Default::default()
            },
        )
    }

    fn default_ipv6_filter_table() -> (IpTablesName, Self) {
        let mut table_name = [0; TABLE_NAME_LEN];
        write_string_to_ascii_buffer("filter".to_owned(), &mut table_name)
            .expect("convert \"filter\" to ASCII");

        let hook_entry =
            FILTER_HOOKS.map(|n| n * u32::from(IP6T_ENTRY_SIZE + STANDARD_TARGET_SIZE));
        let mut entries = Vec::new();
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::end_of_input_v6());

        (
            table_name,
            Self {
                valid_hooks: NfIpHooks::FILTER.bits(),
                hook_entry,
                underflow: hook_entry,
                num_entries: 4,
                size: entries.len() as u32,
                entries,
                ..Default::default()
            },
        )
    }

    fn default_ipv4_mangle_table() -> (IpTablesName, Self) {
        let mut table_name = [0; TABLE_NAME_LEN];
        write_string_to_ascii_buffer("mangle".to_owned(), &mut table_name)
            .expect("convert \"mangle\" to ASCII");

        let hook_entry = MANGLE_HOOKS.map(|n| n * u32::from(IPT_ENTRY_SIZE + STANDARD_TARGET_SIZE));
        let mut entries = Vec::new();
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::end_of_input_v4());

        (
            table_name,
            Self {
                valid_hooks: NfIpHooks::MANGLE.bits(),
                hook_entry,
                underflow: hook_entry,
                num_entries: 6,
                size: entries.len() as u32,
                entries,
                ..Default::default()
            },
        )
    }

    fn default_ipv6_mangle_table() -> (IpTablesName, Self) {
        let mut table_name = [0; TABLE_NAME_LEN];
        write_string_to_ascii_buffer("mangle".to_owned(), &mut table_name)
            .expect("convert \"mangle\" to ASCII");

        let hook_entry =
            MANGLE_HOOKS.map(|n| n * u32::from(IP6T_ENTRY_SIZE + STANDARD_TARGET_SIZE));
        let mut entries = Vec::new();
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::end_of_input_v6());

        (
            table_name,
            Self {
                valid_hooks: NfIpHooks::MANGLE.bits(),
                hook_entry,
                underflow: hook_entry,
                num_entries: 6,
                size: entries.len() as u32,
                entries,
                ..Default::default()
            },
        )
    }

    fn default_ipv4_raw_table() -> (IpTablesName, Self) {
        let mut table_name = [0; TABLE_NAME_LEN];
        write_string_to_ascii_buffer("raw".to_owned(), &mut table_name)
            .expect("convert \"raw\" to ASCII");

        let hook_entry = RAW_HOOKS.map(|n| n * u32::from(IPT_ENTRY_SIZE + STANDARD_TARGET_SIZE));
        let mut entries = Vec::new();
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::accept_policy_v4());
        entries.extend(Self::end_of_input_v4());

        (
            table_name,
            Self {
                valid_hooks: NfIpHooks::RAW.bits(),
                hook_entry,
                underflow: hook_entry,
                num_entries: 3,
                size: entries.len() as u32,
                entries,
                ..Default::default()
            },
        )
    }

    fn default_ipv6_raw_table() -> (IpTablesName, Self) {
        let mut table_name = [0; TABLE_NAME_LEN];
        write_string_to_ascii_buffer("raw".to_owned(), &mut table_name)
            .expect("convert \"raw\" to ASCII");

        let hook_entry = RAW_HOOKS.map(|n| n * u32::from(IP6T_ENTRY_SIZE + STANDARD_TARGET_SIZE));
        let mut entries = Vec::new();
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::accept_policy_v6());
        entries.extend(Self::end_of_input_v6());

        (
            table_name,
            Self {
                valid_hooks: NfIpHooks::RAW.bits(),
                hook_entry,
                underflow: hook_entry,
                num_entries: 3,
                size: entries.len() as u32,
                entries,
                ..Default::default()
            },
        )
    }
}

/// Stores [`IpTable`]s associated with each protocol.
#[derive(Default)]
pub struct IpTables {
    ipv4: HashMap<IpTablesName, IpTable>,
    ipv6: HashMap<IpTablesName, IpTable>,

    /// Controller to configure net filtering state.
    ///
    /// Initialized lazily with `get_controller`.
    controller: Option<Controller>,
}

#[derive(Debug, Error)]
enum GetControllerError {
    #[error("failed to connect to protocol: {0}")]
    ConnectToProtocol(anyhow::Error),
    #[error("failed to create controller: {0}")]
    ControllerCreation(ControllerCreationError),
}

impl IpTables {
    pub fn new() -> Self {
        // Install default chains and policies on supported tables. These chains are expected to be
        // present on the system before `iptables` client is ran.
        // TODO(https://fxbug.dev/354766238): Propagated default rules to fuchsia.net.filter.
        Self {
            ipv4: HashMap::from([
                IpTable::default_ipv4_filter_table(),
                IpTable::default_ipv4_mangle_table(),
                IpTable::default_ipv4_nat_table(),
                IpTable::default_ipv4_raw_table(),
            ]),
            ipv6: HashMap::from([
                IpTable::default_ipv6_filter_table(),
                IpTable::default_ipv6_mangle_table(),
                IpTable::default_ipv6_nat_table(),
                IpTable::default_ipv6_raw_table(),
            ]),
            ..Default::default()
        }
    }

    fn get_controller(&mut self) -> Result<&mut Controller, GetControllerError> {
        if self.controller.is_none() {
            let control_proxy = connect_to_protocol_sync::<fnet_filter::ControlMarker>()
                .map_err(GetControllerError::ConnectToProtocol)?;
            self.controller = Some(
                Controller::new(
                    &control_proxy,
                    &ControllerId(NAMESPACE_ID_PREFIX.to_string()),
                    zx::MonotonicInstant::INFINITE,
                )
                .map_err(GetControllerError::ControllerCreation)?,
            );
        }
        Ok(self.controller.as_mut().expect("just ensured this is Some"))
    }

    /// Returns `true` if the sockopt can be handled by [`IpTables`].
    pub fn can_handle_getsockopt(level: u32, optname: u32) -> bool {
        matches!(
            (level, optname),
            (
                SOL_IP,
                IPT_SO_GET_INFO
                    | IPT_SO_GET_ENTRIES
                    | IPT_SO_GET_REVISION_MATCH
                    | IPT_SO_GET_REVISION_TARGET,
            ) | (
                SOL_IPV6,
                IP6T_SO_GET_INFO
                    | IP6T_SO_GET_ENTRIES
                    | IP6T_SO_GET_REVISION_MATCH
                    | IP6T_SO_GET_REVISION_TARGET,
            )
        )
    }

    /// Returns `true` if the sockopt can be handled by [`IpTables`].
    pub fn can_handle_setsockopt(level: u32, optname: u32) -> bool {
        matches!(
            (level, optname),
            (SOL_IP | SOL_IPV6, IPT_SO_SET_REPLACE | IPT_SO_SET_ADD_COUNTERS)
        )
    }

    pub fn getsockopt(
        &self,
        socket: &SocketHandle,
        optname: u32,
        mut optval: Vec<u8>,
    ) -> Result<Vec<u8>, Errno> {
        if optval.is_empty() {
            return error!(EINVAL);
        }
        if socket.socket_type != SocketType::Raw {
            return error!(ENOPROTOOPT);
        }

        match optname {
            // Returns information about the table specified by `optval`.
            IPT_SO_GET_INFO => {
                if socket.domain == SocketDomain::Inet {
                    let (mut info, _) =
                        ipt_getinfo::read_from_prefix(&*optval).map_err(|_| errno!(EINVAL))?;
                    let table = self.ipv4.get(&info.name);
                    match table {
                        Some(iptable) => {
                            info.valid_hooks = iptable.valid_hooks;
                            info.hook_entry = iptable.hook_entry;
                            info.underflow = iptable.underflow;
                            info.num_entries = iptable.num_entries;
                            info.size = iptable.size;
                            return Ok(info.as_bytes().to_vec());
                        }
                        None => Ok(optval),
                    }
                } else {
                    let (mut info, _) =
                        ip6t_getinfo::read_from_prefix(&*optval).map_err(|_| errno!(EINVAL))?;
                    let table = self.ipv6.get(&info.name);
                    match table {
                        Some(iptable) => {
                            info.valid_hooks = iptable.valid_hooks;
                            info.hook_entry = iptable.hook_entry;
                            info.underflow = iptable.underflow;
                            info.num_entries = iptable.num_entries;
                            info.size = iptable.size;
                            return Ok(info.as_bytes().to_vec());
                        }
                        None => Ok(optval),
                    }
                }
            }

            // Returns the entries of the table specified by `optval`.
            IPT_SO_GET_ENTRIES => {
                if socket.domain == SocketDomain::Inet {
                    let (get_entries, _) =
                        ipt_get_entries::read_from_prefix(&*optval).map_err(|_| errno!(EINVAL))?;
                    let mut entry_bytes = match self.ipv4.get(&get_entries.name) {
                        Some(iptable) => iptable.entries.clone(),
                        None => vec![],
                    };

                    if entry_bytes.len() > get_entries.size as usize {
                        log_warn!("Entries are longer than expected so truncating.");
                        entry_bytes.truncate(get_entries.size as usize);
                    }

                    optval.truncate(std::mem::size_of::<ipt_get_entries>());
                    optval.append(&mut entry_bytes);
                } else {
                    let (get_entries, _) =
                        ip6t_get_entries::read_from_prefix(&*optval).map_err(|_| errno!(EINVAL))?;
                    let mut entry_bytes = match self.ipv6.get(&get_entries.name) {
                        Some(iptable) => iptable.entries.clone(),
                        None => vec![],
                    };

                    if entry_bytes.len() > get_entries.size as usize {
                        log_warn!("Entries are longer than expected so truncating.");
                        entry_bytes.truncate(get_entries.size as usize);
                    }

                    optval.truncate(std::mem::size_of::<ip6t_get_entries>());
                    optval.append(&mut entry_bytes);
                }
                Ok(optval)
            }

            // Returns the revision match. Currently stubbed to return a max version number.
            IPT_SO_GET_REVISION_MATCH | IP6T_SO_GET_REVISION_MATCH => {
                let (mut revision, _) =
                    xt_get_revision::read_from_prefix(&*optval).map_err(|_| errno!(EINVAL))?;
                revision.revision = u8::MAX;
                Ok(revision.as_bytes().to_vec())
            }

            // Returns the revision target. Currently stubbed to return a max version number.
            IPT_SO_GET_REVISION_TARGET | IP6T_SO_GET_REVISION_TARGET => {
                let (mut revision, _) =
                    xt_get_revision::read_from_prefix(&*optval).map_err(|_| errno!(EINVAL))?;
                revision.revision = u8::MAX;
                Ok(revision.as_bytes().to_vec())
            }

            _ => {
                track_stub!(TODO("https://fxbug.dev/322875228"), "optname for network sockets");
                Ok(vec![])
            }
        }
    }

    pub fn setsockopt(
        &mut self,
        current_task: &CurrentTask,
        socket: &SocketHandle,
        optname: u32,
        user_opt: UserBuffer,
    ) -> Result<(), Errno> {
        let mut bytes = current_task.read_buffer(&user_opt)?;
        match optname {
            // Replaces the [`IpTable`] specified by `user_opt`.
            IPT_SO_SET_REPLACE => {
                if socket.domain == SocketDomain::Inet {
                    self.replace_ipv4_table(bytes)
                } else {
                    self.replace_ipv6_table(bytes)
                }
            }

            // Sets the counters of the [`IpTable`] specified by `user_opt`.
            IPT_SO_SET_ADD_COUNTERS => {
                let (counters_info, _) =
                    xt_counters_info::read_from_prefix(&*bytes).map_err(|_| errno!(EINVAL))?;

                if let Some(entry) = match socket.domain {
                    SocketDomain::Inet => self.ipv4.get_mut(&counters_info.name),
                    _ => self.ipv6.get_mut(&counters_info.name),
                } {
                    entry.num_counters = counters_info.num_counters;
                    let mut counters = vec![];
                    bytes = bytes.split_off(std::mem::size_of::<xt_counters_info>());
                    for chunk in bytes.chunks(std::mem::size_of::<xt_counters>()) {
                        counters.push(
                            xt_counters::read_from_prefix(chunk).map_err(|_| errno!(EINVAL))?.0,
                        );
                    }
                    entry.counters = counters;
                    return Ok(());
                }
                error!(EINVAL)
            }

            _ => Ok(()),
        }
    }

    fn replace_ipv4_table(&mut self, bytes: Vec<u8>) -> Result<(), Errno> {
        let table = iptables_utils::IpTable::from_ipt_replace(bytes).map_err(|e| {
            log_warn!("Iptables: encountered error while parsing rules: {e}");
            errno!(EINVAL)
        })?;
        let entries = table.parser.entries_bytes().to_vec();
        let replace_info = table.parser.replace_info.clone();
        let mut name: IpTablesName = [0; 32usize];
        write_string_to_ascii_buffer(replace_info.name, &mut name).map_err(|_| errno!(EINVAL))?;
        let iptable_entry = IpTable {
            num_entries: replace_info.num_entries as u32,
            size: replace_info.size as u32,
            entries,
            num_counters: replace_info.num_counters,
            valid_hooks: replace_info.valid_hooks.bits(),
            hook_entry: replace_info.hook_entry,
            underflow: replace_info.underflow,
            counters: vec![],
        };

        self.send_changes_to_net_filter(table.into_changes())?;
        self.ipv4.insert(name, iptable_entry);

        Ok(())
    }

    fn replace_ipv6_table(&mut self, bytes: Vec<u8>) -> Result<(), Errno> {
        let table = iptables_utils::IpTable::from_ip6t_replace(bytes).map_err(|e| {
            log_warn!("Iptables: encountered error while parsing rules: {e}");
            errno!(EINVAL)
        })?;
        let entries = table.parser.entries_bytes().to_vec();
        let replace_info = table.parser.replace_info.clone();
        let mut name: IpTablesName = [0; 32usize];
        write_string_to_ascii_buffer(replace_info.name, &mut name).map_err(|_| errno!(EINVAL))?;
        let iptable_entry = IpTable {
            num_entries: replace_info.num_entries as u32,
            size: replace_info.size as u32,
            entries,
            num_counters: replace_info.num_counters,
            valid_hooks: replace_info.valid_hooks.bits(),
            hook_entry: replace_info.hook_entry,
            underflow: replace_info.underflow,
            counters: vec![],
        };

        self.send_changes_to_net_filter(table.into_changes())?;
        self.ipv6.insert(name, iptable_entry);

        Ok(())
    }

    fn send_changes_to_net_filter(
        &mut self,
        changes: impl Iterator<Item = Change>,
    ) -> Result<(), Errno> {
        match self.get_controller() {
            Err(e) => {
                log_warn!(
                    "IpTables: could not connect to fuchsia.net.filter.NamespaceController: {e}"
                );
            }
            Ok(controller) => {
                for chunk in &changes.chunks(fnet_filter::MAX_BATCH_SIZE as usize) {
                    match controller.push_changes(chunk.collect(), zx::MonotonicInstant::INFINITE) {
                        Ok(()) => {}
                        Err(
                            e @ (PushChangesError::CallMethod(_)
                            | PushChangesError::TooManyChanges
                            | PushChangesError::FidlConversion(_)),
                        ) => {
                            log_warn!(
                                "IpTables: failed to call \
                                fuchsia.net.filter.NamespaceController/PushChanges: {e}"
                            );
                            return error!(ECOMM);
                        }
                        Err(e @ PushChangesError::ErrorOnChange(_)) => {
                            log_warn!(
                                "IpTables: fuchsia.net.filter.NamespaceController/PushChanges \
                                returned error: {e}"
                            );
                            return error!(EINVAL);
                        }
                    }
                }

                match controller.commit_idempotent(zx::MonotonicInstant::INFINITE) {
                    Ok(()) => {}
                    Err(e @ (CommitError::CallMethod(_) | CommitError::FidlConversion(_))) => {
                        log_warn!(
                            "IpTables: failed to call \
                            fuchsia.net.filter.NamespaceController/Commit: {e}"
                        );
                        return error!(ECOMM);
                    }
                    Err(
                        e @ (CommitError::RuleWithInvalidMatcher(_)
                        | CommitError::RuleWithInvalidAction(_)
                        | CommitError::TransparentProxyWithInvalidMatcher(_)
                        | CommitError::CyclicalRoutineGraph(_)
                        | CommitError::RedirectWithInvalidMatcher(_)
                        | CommitError::MasqueradeWithInvalidMatcher(_)
                        | CommitError::ErrorOnChange(_)),
                    ) => {
                        log_warn!(
                            "IpTables: fuchsia.net.filter.NamespaceController/Commit \
                            returned error: {e}"
                        );
                        return error!(EINVAL);
                    }
                }
            }
        };
        Ok(())
    }
}
