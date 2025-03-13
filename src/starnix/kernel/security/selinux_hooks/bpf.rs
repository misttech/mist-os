// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use super::{check_permission, check_self_permission, BpfMapState, BpfProgState};

use crate::bpf::BpfMap;
use crate::security::PermissionFlags;
use crate::task::CurrentTask;
use selinux::{BpfPermission, SecurityId, SecurityServer};
use starnix_uapi::errors::Errno;
use starnix_uapi::{bpf_cmd, bpf_cmd_BPF_MAP_CREATE, bpf_cmd_BPF_PROG_LOAD, bpf_cmd_BPF_PROG_RUN};
use zerocopy::FromBytes;

/// Returns the security state to be assigned to a BPF map. This is defined as the security
/// context of the creating task.
pub fn bpf_map_alloc(current_task: &CurrentTask) -> BpfMapState {
    BpfMapState { sid: current_task.security_state.lock().current_sid }
}

/// Returns the security state to be assigned to a BPF program. This is defined as the
/// security context of the creating task.
pub fn bpf_prog_alloc(current_task: &CurrentTask) -> BpfProgState {
    BpfProgState { sid: current_task.security_state.lock().current_sid }
}

/// Returns whether `current_task` can perform the bpf `cmd`.
pub fn check_bpf_access<Attr: FromBytes>(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    cmd: bpf_cmd,
    _attr: &Attr,
    _attr_size: u32,
) -> Result<(), Errno> {
    let audit_context = current_task.into();

    let sid: SecurityId = current_task.security_state.lock().current_sid;
    let permission = match cmd {
        bpf_cmd_BPF_MAP_CREATE => BpfPermission::MapCreate,
        bpf_cmd_BPF_PROG_LOAD => BpfPermission::ProgLoad,
        bpf_cmd_BPF_PROG_RUN => BpfPermission::ProgRun,
        _ => return Ok(()),
    };
    check_self_permission(&security_server.as_permission_check(), sid, permission, audit_context)
}

/// Returns whether `current_task` can create a bpf_map.
pub fn check_bpf_map_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    bpf_map: &BpfMap,
    flags: PermissionFlags,
) -> Result<(), Errno> {
    let audit_context = current_task.into();

    let subject_sid = current_task.security_state.lock().current_sid;
    let mut permissions = Vec::new();
    if flags.contains(PermissionFlags::READ) {
        permissions.push(BpfPermission::MapRead);
    }
    if flags.contains(PermissionFlags::WRITE) {
        permissions.push(BpfPermission::MapWrite);
    }
    for permission in permissions {
        check_permission(
            &security_server.as_permission_check(),
            subject_sid,
            bpf_map.security_state.state.sid,
            permission,
            audit_context,
        )?;
    }
    Ok(())
}
