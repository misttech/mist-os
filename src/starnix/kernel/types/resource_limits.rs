// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use crate::types::*;

/// A description of a resource.
pub struct ResourceDesc {
    /// The name of the resource.
    pub name: &'static str,

    /// The units in which limits on the resource are expressed.
    pub unit: &'static str,
}

impl ResourceDesc {
    fn new(name: &'static str, unit: &'static str) -> ResourceDesc {
        ResourceDesc { name, unit }
    }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    CPU,
    FSIZE,
    DATA,
    STACK,
    CORE,
    RSS,
    NPROC,
    NOFILE,
    MEMLOCK,
    AS,
    LOCKS,
    SIGPENDING,
    MSGQUEUE,
    NICE,
    RTPRIO,
    RTTIME,
}

impl Resource {
    pub const ALL: [Resource; 16] = [
        Resource::CPU,
        Resource::FSIZE,
        Resource::DATA,
        Resource::STACK,
        Resource::CORE,
        Resource::RSS,
        Resource::NPROC,
        Resource::NOFILE,
        Resource::MEMLOCK,
        Resource::AS,
        Resource::LOCKS,
        Resource::SIGPENDING,
        Resource::MSGQUEUE,
        Resource::NICE,
        Resource::RTPRIO,
        Resource::RTTIME,
    ];

    pub fn from_raw(raw: u32) -> Result<Resource, Errno> {
        Ok(match raw {
            RLIMIT_CPU => Resource::CPU,
            RLIMIT_FSIZE => Resource::FSIZE,
            RLIMIT_DATA => Resource::DATA,
            RLIMIT_STACK => Resource::STACK,
            RLIMIT_CORE => Resource::CORE,
            RLIMIT_RSS => Resource::RSS,
            RLIMIT_NPROC => Resource::NPROC,
            RLIMIT_NOFILE => Resource::NOFILE,
            RLIMIT_MEMLOCK => Resource::MEMLOCK,
            RLIMIT_AS => Resource::AS,
            RLIMIT_LOCKS => Resource::LOCKS,
            RLIMIT_SIGPENDING => Resource::SIGPENDING,
            RLIMIT_MSGQUEUE => Resource::MSGQUEUE,
            RLIMIT_NICE => Resource::NICE,
            RLIMIT_RTPRIO => Resource::RTPRIO,
            RLIMIT_RTTIME => Resource::RTTIME,
            _ => return error!(EINVAL),
        })
    }

    pub fn desc(&self) -> ResourceDesc {
        match self {
            Resource::CPU => ResourceDesc::new("Max cpu time", "seconds"),
            Resource::FSIZE => ResourceDesc::new("Max file size", "bytes"),
            Resource::DATA => ResourceDesc::new("Max data size", "bytes"),
            Resource::STACK => ResourceDesc::new("Max stack size", "bytes"),
            Resource::CORE => ResourceDesc::new("Max core file size", "bytes"),
            Resource::RSS => ResourceDesc::new("Max resident set", "bytes"),
            Resource::NPROC => ResourceDesc::new("Max processes", "processes"),
            Resource::NOFILE => ResourceDesc::new("Max open files", "files"),
            Resource::MEMLOCK => ResourceDesc::new("Max locked memory", "bytes"),
            Resource::AS => ResourceDesc::new("Max address space", "bytes"),
            Resource::LOCKS => ResourceDesc::new("Max file locks", "bytes"),
            Resource::SIGPENDING => ResourceDesc::new("Max pending signals", "signals"),
            Resource::MSGQUEUE => ResourceDesc::new("Max msgqueue size", "bytes"),
            Resource::NICE => ResourceDesc::new("Max nice priority", ""),
            Resource::RTPRIO => ResourceDesc::new("Max realtime priority", ""),
            Resource::RTTIME => ResourceDesc::new("Max realtime timeout", "us"),
        }
    }
}

#[derive(Default)]
pub struct ResourceLimits {
    values: HashMap<Resource, rlimit>,
}

const INFINITE_LIMIT: rlimit =
    rlimit { rlim_cur: RLIM_INFINITY as u64, rlim_max: RLIM_INFINITY as u64 };

impl ResourceLimits {
    pub fn get(&self, resource: Resource) -> rlimit {
        *self.values.get(&resource).unwrap_or(&INFINITE_LIMIT)
    }

    pub fn set(&mut self, resource: Resource, value: rlimit) {
        self.values.insert(resource, value);
    }
}
