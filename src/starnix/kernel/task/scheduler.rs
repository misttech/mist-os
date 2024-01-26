// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::HandleBased;
use fidl_fuchsia_scheduler::ProfileProviderSynchronousProxy;
use fuchsia_zircon as zx;
use starnix_logging::{impossible_error, log_debug, log_warn, track_stub};
use starnix_uapi::{
    errno, error, errors::Errno, sched_param, SCHED_BATCH, SCHED_DEADLINE, SCHED_FIFO, SCHED_IDLE,
    SCHED_NORMAL, SCHED_RESET_ON_FORK, SCHED_RR,
};
use std::cmp::Ordering;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct SchedulerPolicy {
    kind: SchedulerPolicyKind,
    reset_on_fork: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SchedulerPolicyKind {
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
        priority: u8,
    },
    RoundRobin {
        /// 0-99, from sched_setpriority()
        priority: u8,
    },
}

impl PartialOrd for SchedulerPolicyKind {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.ordering().partial_cmp(&other.ordering())
    }
}

impl std::default::Default for SchedulerPolicyKind {
    fn default() -> Self {
        Self::Normal { priority: DEFAULT_TASK_PRIORITY }
    }
}

impl SchedulerPolicy {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn from_raw(mut policy: u32, params: sched_param, rlimit: u64) -> Result<Self, Errno> {
        let reset_on_fork = (policy & SCHED_RESET_ON_FORK) != 0;
        if reset_on_fork {
            track_stub!(
                TODO("https://fxbug.dev/297961833"),
                "SCHED_RESET_ON_FORK check CAP_SYS_NICE"
            );
            policy -= SCHED_RESET_ON_FORK;
        }

        let valid_priorities =
            min_priority_for_sched_policy(policy)?..=max_priority_for_sched_policy(policy)?;
        if !valid_priorities.contains(&params.sched_priority) {
            return error!(EINVAL);
        }
        // ok to cast i32->u64, above range excludes negatives
        let kind = match (policy, params.sched_priority as u64) {
            (SCHED_FIFO, priority) => {
                Ok(SchedulerPolicyKind::Fifo { priority: std::cmp::min(priority, rlimit) as u8 })
            }
            (SCHED_RR, priority) => Ok(SchedulerPolicyKind::RoundRobin {
                priority: std::cmp::min(priority, rlimit) as u8,
            }),

            // Non-real-time scheduler policies are not allowed to set their nice value from
            // the sched_priority field.
            (SCHED_NORMAL, 0) => {
                Ok(SchedulerPolicyKind::Normal { priority: DEFAULT_TASK_PRIORITY })
            }
            (SCHED_BATCH, 0) => Ok(SchedulerPolicyKind::Batch { priority: DEFAULT_TASK_PRIORITY }),
            (SCHED_IDLE, 0) => Ok(SchedulerPolicyKind::Idle { priority: DEFAULT_TASK_PRIORITY }),
            _ => error!(EINVAL),
        }?;

        Ok(Self { kind, reset_on_fork })
    }

    pub fn fork(self) -> Self {
        if self.reset_on_fork {
            Self {
                kind: match self.kind {
                    // If the calling thread has a scheduling policy of SCHED_FIFO or
                    // SCHED_RR, the policy is reset to SCHED_OTHER in child processes.
                    SchedulerPolicyKind::Fifo { .. } | SchedulerPolicyKind::RoundRobin { .. } => {
                        SchedulerPolicyKind::default()
                    }

                    // If the calling process has a negative nice value, the nice
                    // value is reset to zero in child processes.
                    SchedulerPolicyKind::Normal { .. } => {
                        SchedulerPolicyKind::Normal { priority: DEFAULT_TASK_PRIORITY }
                    }
                    SchedulerPolicyKind::Batch { .. } => {
                        SchedulerPolicyKind::Batch { priority: DEFAULT_TASK_PRIORITY }
                    }
                    SchedulerPolicyKind::Idle { .. } => {
                        SchedulerPolicyKind::Idle { priority: DEFAULT_TASK_PRIORITY }
                    }
                },
                // This flag is disabled in child processes created by fork(2).
                reset_on_fork: false,
            }
        } else {
            self
        }
    }

    pub fn raw_policy(&self) -> u32 {
        let mut base = match self.kind {
            SchedulerPolicyKind::Normal { .. } => SCHED_NORMAL,
            SchedulerPolicyKind::Batch { .. } => SCHED_BATCH,
            SchedulerPolicyKind::Idle { .. } => SCHED_IDLE,
            SchedulerPolicyKind::Fifo { .. } => SCHED_FIFO,
            SchedulerPolicyKind::RoundRobin { .. } => SCHED_RR,
        };
        if self.reset_on_fork {
            base |= SCHED_RESET_ON_FORK;
        }
        base
    }

    /// Return the raw "normal priority" for a process, in the range 1-40. This is the value used to
    /// compute nice, and does not apply to real-time scheduler policies.
    pub fn raw_priority(&self) -> u8 {
        match self.kind {
            SchedulerPolicyKind::Normal { priority }
            | SchedulerPolicyKind::Batch { priority }
            | SchedulerPolicyKind::Idle { priority } => priority,
            _ => DEFAULT_TASK_PRIORITY,
        }
    }

    /// Set the "normal priority" for a process, in the range 1-40. This is the value used to
    /// compute nice, and does not apply to real-time scheduler policies.
    pub fn set_raw_nice(&mut self, new_priority: u8) {
        match &mut self.kind {
            SchedulerPolicyKind::Normal { priority }
            | SchedulerPolicyKind::Batch { priority }
            | SchedulerPolicyKind::Idle { priority } => *priority = new_priority,
            _ => (),
        }
    }

    pub fn raw_params(&self) -> sched_param {
        match self.kind {
            SchedulerPolicyKind::Normal { .. }
            | SchedulerPolicyKind::Batch { .. }
            | SchedulerPolicyKind::Idle { .. } => sched_param { sched_priority: 0 },
            SchedulerPolicyKind::Fifo { priority }
            | SchedulerPolicyKind::RoundRobin { priority } => {
                sched_param { sched_priority: priority as i32 }
            }
        }
    }
}

impl SchedulerPolicyKind {
    /// Returns a tuploe allowing to compare 2 policies.
    fn ordering(&self) -> (u8, u8) {
        match self {
            Self::RoundRobin { priority } | Self::Fifo { priority } => (3, *priority),
            Self::Normal { priority } => (2, *priority),
            Self::Batch { priority } => (1, *priority),
            Self::Idle { priority } => (0, *priority),
        }
    }

    /// Returns a number 0-31 (inclusive) mapping Linux scheduler priority to a Zircon priority
    /// level for the fair scheduler.
    ///
    /// The range of 32 Zircon priorities is divided into a region for each flavor of Linux
    /// scheduling:
    ///
    /// 1. 0-3 (inclusive) is used for SCHED_IDLE, the lowest priority Linux tasks.
    /// 2. 6-15 (inclusive) is used for lower-than-default-priority SCHED_OTHER/SCHED_BATCH tasks.
    /// 3. 16 is used for the default priority SCHED_OTHER/SCHED_BATCH, the same as Zircon's
    ///    default for Fuchsia processes.
    /// 4. 17-26 (inclusive) is used for higher-than-default-priority SCHED_OTHER/SCHED_BATCH tasks.
    /// 5. 28-31 (inclusive) is used to temporarily emulate aggressive preemption for SCHED_FIFO/SCHED_RR
    ///    tasks, offering enough values to differentiate between the static priorities used in
    ///    prioritized workloads at time of writing (1, 2, and 99).
    fn zircon_fair_priority(&self) -> u8 {
        match self {
            // Configured with nice 0-40, mapped to 0-3.
            Self::Idle { priority } => priority / 11,

            // Configured with nice 0-40 and mapped to 6-26. 20 is the default nice which we want to
            // map to 16.
            Self::Normal { priority } => (priority / 2) + 6,
            Self::Batch { priority } => {
                track_stub!(TODO("https://fxbug.dev/308055542"), "SCHED_BATCH hinting");
                (priority / 2) + 6
            }

            // Configured with priority 1-99, mapped to 28-31.
            Self::Fifo { priority } | Self::RoundRobin { priority } => {
                track_stub!(TODO("https://fxbug.dev/308055654"), "real SCHED_FIFO/SCHED_RR");
                match priority {
                    1 => 29,
                    2 => 30,
                    _ => 31,
                }
            }
        }
    }

    fn role_name(&self) -> &'static str {
        FAIR_PRIORITY_ROLE_NAMES[self.zircon_fair_priority() as usize]
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

pub fn set_thread_role(
    profile_provider: &ProfileProviderSynchronousProxy,
    thread: &zx::Thread,
    policy: SchedulerPolicy,
) -> Result<(), Errno> {
    let role_name = policy.kind.role_name();
    log_debug!(?policy, role_name, "setting thread role");
    let thread = thread.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(impossible_error)?;
    profile_provider
        .set_profile_by_role(thread.into_handle(), role_name, zx::Time::INFINITE)
        .map_err(|err| {
            log_warn!(?err, "Unable to set thread profile.");
            errno!(EINVAL)
        })?;
    Ok(())
}

/// Names of ProfileProvider roles for each static Zircon priority in the fair scheduler.
/// The index in the array is equal to the static priority.
// LINT.IfChange
const FAIR_PRIORITY_ROLE_NAMES: [&str; 32] = [
    "fuchsia.starnix.fair.0",
    "fuchsia.starnix.fair.1",
    "fuchsia.starnix.fair.2",
    "fuchsia.starnix.fair.3",
    "fuchsia.starnix.fair.4",
    "fuchsia.starnix.fair.5",
    "fuchsia.starnix.fair.6",
    "fuchsia.starnix.fair.7",
    "fuchsia.starnix.fair.8",
    "fuchsia.starnix.fair.9",
    "fuchsia.starnix.fair.10",
    "fuchsia.starnix.fair.11",
    "fuchsia.starnix.fair.12",
    "fuchsia.starnix.fair.13",
    "fuchsia.starnix.fair.14",
    "fuchsia.starnix.fair.15",
    "fuchsia.starnix.fair.16",
    "fuchsia.starnix.fair.17",
    "fuchsia.starnix.fair.18",
    "fuchsia.starnix.fair.19",
    "fuchsia.starnix.fair.20",
    "fuchsia.starnix.fair.21",
    "fuchsia.starnix.fair.22",
    "fuchsia.starnix.fair.23",
    "fuchsia.starnix.fair.24",
    "fuchsia.starnix.fair.25",
    "fuchsia.starnix.fair.26",
    "fuchsia.starnix.fair.27",
    "fuchsia.starnix.fair.28",
    "fuchsia.starnix.fair.29",
    "fuchsia.starnix.fair.30",
    "fuchsia.starnix.fair.31",
];
// LINT.ThenChange(src/starnix/config/starnix.profiles)

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn default_role_name() {
        assert_eq!(SchedulerPolicyKind::default().role_name(), "fuchsia.starnix.fair.16");
    }

    #[fuchsia::test]
    fn normal_with_non_default_nice_role_name() {
        assert_eq!(
            SchedulerPolicyKind::Normal { priority: 10 }.role_name(),
            "fuchsia.starnix.fair.11"
        );
        assert_eq!(
            SchedulerPolicyKind::Normal { priority: 27 }.role_name(),
            "fuchsia.starnix.fair.19"
        );
    }

    #[fuchsia::test]
    fn fifo_role_name() {
        assert_eq!(
            SchedulerPolicyKind::Fifo { priority: 1 }.role_name(),
            "fuchsia.starnix.fair.29"
        );
        assert_eq!(
            SchedulerPolicyKind::Fifo { priority: 2 }.role_name(),
            "fuchsia.starnix.fair.30"
        );
        assert_eq!(
            SchedulerPolicyKind::Fifo { priority: 99 }.role_name(),
            "fuchsia.starnix.fair.31"
        );
    }
}
