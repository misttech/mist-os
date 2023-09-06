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
// In POSIX, priority is a per-process setting, but in Linux it is per-thread.
// See https://man7.org/linux/man-pages/man2/setpriority.2.html#BUGS and
// https://man7.org/linux/man-pages/man2/setpriority.2.html#NOTES
const DEFAULT_TASK_PRIORITY: u8 = 20;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SchedulerPolicy {
    Normal {
        // 1-40, from setpriority()
        priority: u8,
    },
    Batch {
        // 1-40, from setpriority()
        priority: u8,
    },
    Idle {
        // 1-40, from setpriority()
        priority: u8,
    },
    Fifo {
        /// 0-99, from sched_setpriority()
        priority: u32,
    },
    RoundRobin {
        /// 0-99, from sched_setpriority()
        priority: u32,
    },
}

impl std::default::Default for SchedulerPolicy {
    fn default() -> Self {
        Self::Normal { priority: DEFAULT_TASK_PRIORITY }
    }
}

impl SchedulerPolicy {
    pub fn is_default(&self) -> bool {
        matches!(self, Self::Normal { priority: DEFAULT_TASK_PRIORITY })
    }

    pub fn from_raw(policy: u32, params: sched_param, rlimit: u64) -> Result<Self, Errno> {
        let valid_priorities =
            min_priority_for_sched_policy(policy)?..=max_priority_for_sched_policy(policy)?;
        if !valid_priorities.contains(&params.sched_priority) {
            return error!(EINVAL);
        }
        // ok to cast i32->u64, above range excludes negatives
        match (policy, params.sched_priority as u64) {
            (SCHED_FIFO, priority) => {
                Ok(Self::Fifo { priority: std::cmp::min(priority, rlimit) as u32 })
            }
            (SCHED_RR, priority) => {
                Ok(Self::RoundRobin { priority: std::cmp::min(priority, rlimit) as u32 })
            }

            // Non-real-time scheduler policies are not allowed to set their nice value from
            // the sched_priority field.
            (SCHED_NORMAL, 0) => Ok(Self::Normal { priority: DEFAULT_TASK_PRIORITY }),
            (SCHED_BATCH, 0) => Ok(Self::Batch { priority: DEFAULT_TASK_PRIORITY }),
            (SCHED_IDLE, 0) => Ok(Self::Idle { priority: DEFAULT_TASK_PRIORITY }),
            _ => error!(EINVAL),
        }
    }

    pub fn raw_policy(&self) -> u32 {
        match self {
            Self::Normal { .. } => SCHED_NORMAL,
            Self::Batch { .. } => SCHED_BATCH,
            Self::Idle { .. } => SCHED_IDLE,
            Self::Fifo { .. } => SCHED_FIFO,
            Self::RoundRobin { .. } => SCHED_RR,
        }
    }

    /// Return the raw "normal priority" for a process, in the range 1-40. This is the value used to
    /// compute nice, and does not apply to real-time scheduler policies.
    pub fn raw_priority(&self) -> u8 {
        match self {
            Self::Normal { priority } | Self::Batch { priority } | Self::Idle { priority } => {
                *priority
            }
            _ => DEFAULT_TASK_PRIORITY,
        }
    }

    /// Set the "normal priority" for a process, in the range 1-40. This is the value used to
    /// compute nice, and does not apply to real-time scheduler policies.
    pub fn set_raw_priority(&mut self, new_priority: u8) {
        match self {
            Self::Normal { priority } | Self::Batch { priority } | Self::Idle { priority } => {
                *priority = new_priority
            }
            _ => (),
        }
    }

    pub fn raw_params(&self) -> sched_param {
        match self {
            Self::Normal { .. } | Self::Batch { .. } | Self::Idle { .. } => {
                sched_param { sched_priority: 0 }
            }
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
