// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    mm::MemoryAccessorExt,
    task::CurrentTask,
    vfs::socket::{iptables_utils, SocketDomain, SocketHandle, SocketType},
};
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext::{
    sync::Controller, Change, CommitError, ControllerCreationError, ControllerId, Namespace,
    PushChangesError, Resource, ResourceId, Routine,
};
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_zircon as zx;
use itertools::Itertools;
use starnix_logging::{log_debug, log_warn, track_stub};
use starnix_uapi::{
    c_char, errno, error, errors::Errno, ip6t_get_entries, ip6t_getinfo, ip6t_replace,
    ipt_get_entries, ipt_getinfo, ipt_replace, nf_inet_hooks_NF_INET_NUMHOOKS,
    user_buffer::UserBuffer, xt_counters, xt_counters_info, xt_get_revision, IP6T_SO_GET_ENTRIES,
    IP6T_SO_GET_INFO, IP6T_SO_GET_REVISION_MATCH, IP6T_SO_GET_REVISION_TARGET, IPT_SO_GET_ENTRIES,
    IPT_SO_GET_INFO, IPT_SO_GET_REVISION_MATCH, IPT_SO_GET_REVISION_TARGET,
    IPT_SO_SET_ADD_COUNTERS, IPT_SO_SET_REPLACE, SOL_IP, SOL_IPV6,
};
use std::collections::HashMap;
use thiserror::Error;
use zerocopy::{AsBytes, FromBytes};

const NAMESPACE_ID_PREFIX: &str = "starnix";

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

type IpTablesName = [c_char; 32usize];

/// Stores [`IpTable`]s associated with each protocol.
#[derive(Default)]
pub struct IpTables {
    ipv4: HashMap<IpTablesName, IpTable>,
    ipv6: HashMap<IpTablesName, IpTable>,

    // Controller to configure net filtering state.
    //
    // Initialized lazily with `get_controller`.
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
        Self::default()
    }

    fn get_controller(&mut self) -> Result<&mut Controller, GetControllerError> {
        Ok(self.controller.get_or_insert({
            let control_proxy = connect_to_protocol_sync::<fnet_filter::ControlMarker>()
                .map_err(GetControllerError::ConnectToProtocol)?;
            Controller::new(
                &control_proxy,
                &ControllerId(NAMESPACE_ID_PREFIX.to_string()),
                zx::Time::INFINITE,
            )
            .map_err(GetControllerError::ControllerCreation)?
        }))
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
                    let mut info =
                        ipt_getinfo::read_from_prefix(&*optval).ok_or_else(|| errno!(EINVAL))?;
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
                    let mut info =
                        ip6t_getinfo::read_from_prefix(&*optval).ok_or_else(|| errno!(EINVAL))?;
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
                    let get_entries = ipt_get_entries::read_from_prefix(&*optval)
                        .ok_or_else(|| errno!(EINVAL))?;
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
                    let get_entries = ip6t_get_entries::read_from_prefix(&*optval)
                        .ok_or_else(|| errno!(EINVAL))?;
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
                let mut revision =
                    xt_get_revision::read_from_prefix(&*optval).ok_or_else(|| errno!(EINVAL))?;
                revision.revision = u8::MAX;
                Ok(revision.as_bytes().to_vec())
            }

            // Returns the revision target. Currently stubbed to return a max version number.
            IPT_SO_GET_REVISION_TARGET | IP6T_SO_GET_REVISION_TARGET => {
                let mut revision =
                    xt_get_revision::read_from_prefix(&*optval).ok_or_else(|| errno!(EINVAL))?;
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
                    let table =
                        ip6t_replace::read_from_prefix(&*bytes).ok_or_else(|| errno!(EINVAL))?;
                    let entries = bytes[std::mem::size_of::<ip6t_replace>()..].to_vec();

                    let entry = IpTable {
                        valid_hooks: table.valid_hooks,
                        hook_entry: table.hook_entry,
                        underflow: table.underflow,
                        num_entries: table.num_entries,
                        size: table.size,
                        entries,
                        num_counters: table.num_counters,
                        counters: vec![],
                    };
                    self.ipv6.insert(table.name, entry);

                    Ok(())
                }
            }

            // Sets the counters of the [`IpTable`] specified by `user_opt`.
            IPT_SO_SET_ADD_COUNTERS => {
                let counters_info =
                    xt_counters_info::read_from_prefix(&*bytes).ok_or_else(|| errno!(EINVAL))?;

                if let Some(entry) = match socket.domain {
                    SocketDomain::Inet => self.ipv4.get_mut(&counters_info.name),
                    _ => self.ipv6.get_mut(&counters_info.name),
                } {
                    entry.num_counters = counters_info.num_counters;
                    let mut counters = vec![];
                    bytes = bytes.split_off(std::mem::size_of::<xt_counters_info>());
                    for chunk in bytes.chunks(std::mem::size_of::<xt_counters>()) {
                        counters.push(
                            xt_counters::read_from_prefix(chunk).ok_or_else(|| errno!(EINVAL))?,
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

    fn create_changes(
        namespace: Namespace,
        routines: Vec<Routine>,
    ) -> impl Iterator<Item = Change> {
        let namespace_changes = [
            // Firstly, remove the existing table, along with all of its routines and rules.
            // We will call Commit with idempotent=true so that this would succeed even if
            // the table does not exist.
            Change::Remove(ResourceId::Namespace(namespace.id.clone())),
            // Recreate the table.
            Change::Create(Resource::Namespace(namespace)),
        ]
        .into_iter();

        let routine_changes = routines.into_iter().map(Resource::Routine).map(Change::Create);

        namespace_changes.chain(routine_changes)
    }

    fn replace_ipv4_table(&mut self, bytes: Vec<u8>) -> Result<(), Errno> {
        let table = iptables_utils::parse_ipt_replace(&bytes)?;
        let changes = Self::create_changes(table.namespace, table.routines);

        match self.get_controller() {
            Err(e) => {
                log_warn!(
                    "IpTables: could not connect to fuchsia.net.filter.NamespaceController: {e}"
                );
            }
            Ok(controller) => {
                for chunk in &changes.chunks(fnet_filter::MAX_BATCH_SIZE as usize) {
                    match controller.push_changes(chunk.collect(), zx::Time::INFINITE) {
                        Ok(()) => {}
                        Err(
                            e @ (PushChangesError::CallMethod(_)
                            | PushChangesError::TooManyChanges
                            | PushChangesError::FidlConversion(_)),
                        ) => {
                            log_debug!(
                                "IpTables: failed to call \
                                fuchsia.net.filter.NamespaceController/PushChanges: {e}"
                            );
                            return error!(ECOMM);
                        }
                        Err(e @ PushChangesError::ErrorOnChange(_)) => {
                            log_debug!(
                                "IpTables: fuchsia.net.filter.NamespaceController/PushChanges \
                                returned error: {e}"
                            );
                            return error!(EINVAL);
                        }
                    }
                }

                match controller.commit_idempotent(zx::Time::INFINITE) {
                    Ok(()) => {}
                    Err(e @ (CommitError::CallMethod(_) | CommitError::FidlConversion(_))) => {
                        log_debug!(
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
                        | CommitError::ErrorOnChange(_)),
                    ) => {
                        log_debug!(
                            "IpTables: fuchsia.net.filter.NamespaceController/Commit \
                            returned error: {e}"
                        );
                        return error!(EINVAL);
                    }
                }
            }
        };

        self.ipv4.insert(
            table.ipt_replace.name,
            IpTable {
                valid_hooks: table.ipt_replace.valid_hooks,
                hook_entry: table.ipt_replace.hook_entry,
                underflow: table.ipt_replace.underflow,
                num_entries: table.ipt_replace.num_entries,
                size: table.ipt_replace.size,
                entries: bytes[std::mem::size_of::<ipt_replace>()..].to_vec(),
                num_counters: table.ipt_replace.num_counters,
                counters: vec![],
            },
        );

        Ok(())
    }
}
