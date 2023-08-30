// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::types::*;

// In user space, priority (niceness) is an integer from -20..19 (inclusive)
// with the default being 0.
//
// In the kernel it is represented as a range from 1..40 (inclusive).
// The conversion is done by the formula: user_nice = 20 - kernel_nice.
//
// See https://man7.org/linux/man-pages/man2/setpriority.2.html#NOTES
pub(crate) const DEFAULT_TASK_PRIORITY: u8 = 20;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SchedulerPolicy {
    #[default]
    Normal,
    Batch,
    Idle,
    Fifo {
        priority: u32,
    },
    RoundRobin {
        priority: u32,
    },
}

impl SchedulerPolicy {
    pub fn from_raw(policy: u32, params: sched_param, rlimit: u64) -> Result<Self, Errno> {
        let valid_priorities =
            min_priority_for_sched_policy(policy)?..=max_priority_for_sched_policy(policy)?;
        if !valid_priorities.contains(&params.sched_priority) {
            return error!(EINVAL);
        }
        // ok to cast i32->u64, above range excludes negatives
        match (policy, params.sched_priority as u64) {
            (SCHED_NORMAL, 0) => Ok(Self::Normal),
            (SCHED_BATCH, 0) => Ok(Self::Batch),
            (SCHED_IDLE, 0) => Ok(Self::Idle),
            (SCHED_FIFO, priority) => {
                Ok(Self::Fifo { priority: std::cmp::min(priority, rlimit) as u32 })
            }
            (SCHED_RR, priority) => {
                Ok(Self::RoundRobin { priority: std::cmp::min(priority, rlimit) as u32 })
            }
            _ => error!(EINVAL),
        }
    }

    pub fn raw_policy(&self) -> u32 {
        match self {
            Self::Normal => SCHED_NORMAL,
            Self::Batch => SCHED_BATCH,
            Self::Idle => SCHED_IDLE,
            Self::Fifo { .. } => SCHED_FIFO,
            Self::RoundRobin { .. } => SCHED_RR,
        }
    }

    pub fn raw_params(&self) -> sched_param {
        match self {
            Self::Normal | Self::Batch | Self::Idle => sched_param { sched_priority: 0 },
            Self::Fifo { priority } | Self::RoundRobin { priority } => {
                sched_param { sched_priority: *priority as i32 }
            }
        }
    }
}

pub fn min_priority_for_sched_policy(policy: u32) -> Result<i32, Errno> {
    match policy {
        SCHED_NORMAL | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => Ok(0),
        SCHED_FIFO | SCHED_RR => Ok(1),
        _ => error!(EINVAL),
    }
}

pub fn max_priority_for_sched_policy(policy: u32) -> Result<i32, Errno> {
    match policy {
        SCHED_NORMAL | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => Ok(0),
        SCHED_FIFO | SCHED_RR => Ok(99),
        _ => error!(EINVAL),
    }
}
