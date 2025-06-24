// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{RoleOverrides, Task};
use fidl::HandleBased;
use fidl_fuchsia_scheduler::{
    RoleManagerMarker, RoleManagerSetRoleRequest, RoleManagerSynchronousProxy, RoleName, RoleTarget,
};
use fuchsia_component::client::connect_to_protocol_sync;
use starnix_logging::{impossible_error, log_debug, log_warn, track_stub};
use starnix_uapi::errors::Errno;
use starnix_uapi::{
    errno, error, sched_param, SCHED_BATCH, SCHED_DEADLINE, SCHED_FIFO, SCHED_IDLE, SCHED_NORMAL,
    SCHED_RESET_ON_FORK, SCHED_RR,
};

pub struct SchedulerManager {
    role_manager: Option<RoleManagerSynchronousProxy>,
    role_overrides: RoleOverrides,
}

impl SchedulerManager {
    /// Create a new SchedulerManager which will apply any provided `role_overrides` before
    /// computing a role name based on a Task's scheduler state.
    pub fn new(role_overrides: RoleOverrides) -> SchedulerManager {
        let role_manager = connect_to_protocol_sync::<RoleManagerMarker>().unwrap();
        let role_manager = if let Err(e) = Self::set_thread_role_inner(
            &role_manager,
            &*fuchsia_runtime::thread_self(),
            SchedulingPolicy::default().role_name(),
        ) {
            log_debug!("Setting thread role failed ({e:?}), will not set thread priority.");
            None
        } else {
            log_debug!("Thread role set successfully, scheduler manager initialized.");
            Some(role_manager)
        };

        SchedulerManager { role_manager, role_overrides }
    }

    /// Create a new empty SchedulerManager for testing.
    pub fn empty_for_tests() -> Self {
        Self { role_manager: None, role_overrides: RoleOverrides::new().build().unwrap() }
    }

    /// Return the currently active role name for this task. Requires read access to `task`'s state,
    /// should only be called by code which is not already modifying the provided `task`.
    pub fn role_name(&self, task: &Task) -> Result<&str, Errno> {
        let scheduler_state = task.read().scheduler_state;
        self.role_name_inner(task, scheduler_state)
    }

    fn role_name_inner(&self, task: &Task, scheduler_state: SchedulerState) -> Result<&str, Errno> {
        Ok(if scheduler_state.policy.is_realtime() {
            let process_name = task
                .thread_group()
                .read()
                .get_task(task.thread_group().leader)
                .ok_or_else(|| errno!(EINVAL))?
                .command();
            let thread_name = task.command();
            if let Some(name) =
                self.role_overrides.get_role_name(process_name.as_bytes(), thread_name.as_bytes())
            {
                name
            } else {
                scheduler_state.policy.role_name()
            }
        } else {
            scheduler_state.policy.role_name()
        })
    }

    /// Give the provided `task`'s Zircon thread a role.
    ///
    /// Requires passing the current `SchedulerState` so that this can be
    /// performed without touching `task`'s state lock.
    pub fn set_thread_role(
        &self,
        task: &Task,
        scheduler_state: SchedulerState,
    ) -> Result<(), Errno> {
        let Some(role_manager) = self.role_manager.as_ref() else {
            log_debug!("no role manager for setting role");
            return Ok(());
        };

        let role_name = self.role_name_inner(task, scheduler_state)?;
        let thread = task.thread.read();
        let Some(thread) = thread.as_ref() else {
            log_debug!("thread role update requested for task without thread, skipping");
            return Ok(());
        };
        Self::set_thread_role_inner(role_manager, thread, role_name)
    }

    fn set_thread_role_inner(
        role_manager: &RoleManagerSynchronousProxy,
        thread: &zx::Thread,
        role_name: &str,
    ) -> Result<(), Errno> {
        log_debug!(role_name; "setting thread role");

        let thread = thread.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(impossible_error)?;
        let request = RoleManagerSetRoleRequest {
            target: Some(RoleTarget::Thread(thread)),
            role: Some(RoleName { role: role_name.to_string() }),
            ..Default::default()
        };
        let _ = role_manager.set_role(request, zx::MonotonicInstant::INFINITE).map_err(|err| {
            log_warn!(err:%; "Unable to set thread role.");
            errno!(EINVAL)
        })?;
        Ok(())
    }
}

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SchedulerState {
    policy: SchedulingPolicy,
    reset_on_fork: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulingPolicy {
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
        /// 0-99, from sched_setscheduler()
        priority: u8,
    },
    RoundRobin {
        /// 0-99, from sched_setscheduler()
        priority: u8,
    },
}

impl std::default::Default for SchedulingPolicy {
    fn default() -> Self {
        Self::Normal { priority: DEFAULT_TASK_PRIORITY }
    }
}

impl SchedulerState {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn policy(&self) -> SchedulingPolicy {
        self.policy
    }

    fn from_raw(mut policy: u32, priority: u8) -> Result<Self, Errno> {
        let reset_on_fork = (policy & SCHED_RESET_ON_FORK) != 0;
        if reset_on_fork {
            track_stub!(
                TODO("https://fxbug.dev/297961833"),
                "SCHED_RESET_ON_FORK check CAP_SYS_NICE"
            );
            policy -= SCHED_RESET_ON_FORK;
        }
        let policy = match policy {
            SCHED_FIFO => SchedulingPolicy::Fifo { priority },
            SCHED_RR => SchedulingPolicy::RoundRobin { priority },
            SCHED_NORMAL => SchedulingPolicy::Normal { priority },
            SCHED_BATCH => SchedulingPolicy::Batch { priority },
            SCHED_IDLE => SchedulingPolicy::Idle { priority },
            SCHED_DEADLINE => {
                track_stub!(TODO("https://fxbug.dev/409349496"), "SCHED_DEADLINE");
                return error!(EINVAL);
            }
            _ => return error!(EINVAL),
        };

        Ok(Self { policy, reset_on_fork })
    }

    /// Create a policy according to the "sched_policy" and "priority" bits of
    /// a flat_binder_object_flags bitmask (see uapi/linux/android/binder.h).
    ///
    /// It would be very strange for this to need to be called anywhere outside
    /// of our Binder implementation.
    pub fn from_binder(policy: u8, priority_or_niceness: u8) -> Result<Self, Errno> {
        if policy != (SCHED_NORMAL as u8)
            && policy != (SCHED_RR as u8)
            && policy != (SCHED_FIFO as u8)
            && policy != (SCHED_BATCH as u8)
        {
            return error!(EINVAL);
        }
        let priority_or_nonnegative_niceness =
            if policy == (SCHED_NORMAL as u8) || policy == (SCHED_BATCH as u8) {
                let signed_niceness = priority_or_niceness as i8;
                if signed_niceness < -20 || signed_niceness > 19 {
                    return error!(EINVAL);
                }
                (20 - signed_niceness) as u8
            } else {
                if priority_or_niceness < 1 || priority_or_niceness > 99 {
                    return error!(EINVAL);
                }
                priority_or_niceness
            };
        Self::from_raw(policy as u32, priority_or_nonnegative_niceness)
    }

    pub fn from_sched_params(policy: u32, params: sched_param, rlimit: u64) -> Result<Self, Errno> {
        let mut priority = u8::try_from(params.sched_priority).map_err(|_| errno!(EINVAL))?;
        let raw_policy = policy & !SCHED_RESET_ON_FORK;
        let valid_priorities =
            min_priority_for_sched_policy(raw_policy)?..=max_priority_for_sched_policy(raw_policy)?;
        if !valid_priorities.contains(&priority) {
            return error!(EINVAL);
        }
        priority = std::cmp::min(priority as u64, rlimit) as u8;
        Self::from_raw(policy, priority)
    }

    pub fn fork(self) -> Self {
        if self.reset_on_fork {
            Self {
                policy: match self.policy {
                    // If the calling thread has a scheduling policy of SCHED_FIFO or
                    // SCHED_RR, the policy is reset to SCHED_OTHER in child processes.
                    SchedulingPolicy::Fifo { .. } | SchedulingPolicy::RoundRobin { .. } => {
                        SchedulingPolicy::default()
                    }

                    // If the calling process has a negative nice value, the nice
                    // value is reset to zero in child processes.
                    SchedulingPolicy::Normal { .. } => {
                        SchedulingPolicy::Normal { priority: DEFAULT_TASK_PRIORITY }
                    }
                    SchedulingPolicy::Batch { .. } => {
                        SchedulingPolicy::Batch { priority: DEFAULT_TASK_PRIORITY }
                    }
                    SchedulingPolicy::Idle { .. } => {
                        SchedulingPolicy::Idle { priority: DEFAULT_TASK_PRIORITY }
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
        let mut base = match self.policy {
            SchedulingPolicy::Normal { .. } => SCHED_NORMAL,
            SchedulingPolicy::Batch { .. } => SCHED_BATCH,
            SchedulingPolicy::Idle { .. } => SCHED_IDLE,
            SchedulingPolicy::Fifo { .. } => SCHED_FIFO,
            SchedulingPolicy::RoundRobin { .. } => SCHED_RR,
        };
        if self.reset_on_fork {
            base |= SCHED_RESET_ON_FORK;
        }
        base
    }

    /// Return the raw "normal priority" for a process, in the range 1-40. This is the value used to
    /// compute nice, and does not apply to real-time scheduler policies.
    pub fn raw_priority(&self) -> u8 {
        match self.policy {
            SchedulingPolicy::Normal { priority }
            | SchedulingPolicy::Batch { priority }
            | SchedulingPolicy::Idle { priority } => priority,
            _ => DEFAULT_TASK_PRIORITY,
        }
    }

    /// Set the "normal priority" for a process, in the range 1-40. This is the value used to
    /// compute nice, and does not apply to real-time scheduler policies.
    pub fn set_raw_nice(&mut self, new_priority: u8) {
        match &mut self.policy {
            SchedulingPolicy::Normal { priority }
            | SchedulingPolicy::Batch { priority }
            | SchedulingPolicy::Idle { priority } => *priority = new_priority,
            _ => (),
        }
    }

    pub fn raw_params(&self) -> sched_param {
        match self.policy {
            SchedulingPolicy::Normal { .. }
            | SchedulingPolicy::Batch { .. }
            | SchedulingPolicy::Idle { .. } => sched_param { sched_priority: 0 },
            SchedulingPolicy::Fifo { priority } | SchedulingPolicy::RoundRobin { priority } => {
                sched_param { sched_priority: priority as i32 }
            }
        }
    }
}

impl SchedulingPolicy {
    pub fn is_realtime(&self) -> bool {
        match self {
            SchedulingPolicy::Normal { .. }
            | SchedulingPolicy::Batch { .. }
            | SchedulingPolicy::Idle { .. } => false,
            SchedulingPolicy::Fifo { .. } | SchedulingPolicy::RoundRobin { .. } => true,
        }
    }

    /// Returns a number 0-31 (inclusive) mapping Linux scheduler priority to a Zircon priority
    /// level for the fair scheduler.
    ///
    /// The range of 32 Zircon priorities is divided into a region for each flavor of Linux
    /// scheduling:
    ///
    /// 1. 0 is used for SCHED_IDLE, the lowest priority Linux tasks.
    /// 2. 6-15 (inclusive) is used for lower-than-default-priority SCHED_OTHER/SCHED_BATCH tasks.
    /// 3. 16 is used for the default priority SCHED_OTHER/SCHED_BATCH, the same as Zircon's
    ///    default for Fuchsia processes.
    /// 4. 17-26 (inclusive) is used for higher-than-default-priority SCHED_OTHER/SCHED_BATCH tasks.
    /// 5. Realtime tasks receive their own profile name.
    fn role_name(&self) -> &'static str {
        match self {
            // Mapped to 0; see "the [...] nice value has no influence for [the SCHED_IDLE] policy"
            // at sched(7).
            Self::Idle { .. } => FAIR_PRIORITY_ROLE_NAMES[0],

            // Configured with nice 0-40 and mapped to 6-26. 20 is the default nice which we want to
            // map to 16.
            Self::Normal { priority } => FAIR_PRIORITY_ROLE_NAMES[(*priority as usize / 2) + 6],
            Self::Batch { priority } => {
                track_stub!(TODO("https://fxbug.dev/308055542"), "SCHED_BATCH hinting");
                FAIR_PRIORITY_ROLE_NAMES[(*priority as usize / 2) + 6]
            }

            // Configured with priority 1-99, mapped to a constant bandwidth profile. Priority
            // between realtime tasks is ignored because we don't currently have a way to tell the
            // scheduler that a given realtime task is more important than another without
            // specifying an earlier deadline for the higher priority task. We can't specify
            // deadlines at runtime, so we'll treat their priorities all the same.
            Self::Fifo { .. } | Self::RoundRobin { .. } => REALTIME_ROLE_NAME,
        }
    }

    // TODO: https://fxbug.dev/425726327 - better understand what are Binder's requirements when
    // comparing one scheduling with another.
    pub fn is_less_than_for_binder(&self, other: Self) -> bool {
        match self {
            Self::Fifo { priority } | Self::RoundRobin { priority } => match other {
                Self::Fifo { priority: other_priority }
                | Self::RoundRobin { priority: other_priority } => *priority < other_priority,
                Self::Normal { .. } | Self::Batch { .. } | Self::Idle { .. } => false,
            },
            Self::Normal { priority } => match other {
                Self::Fifo { .. } | Self::RoundRobin { .. } => true,
                Self::Normal { priority: other_priority } => *priority < other_priority,
                Self::Batch { .. } | Self::Idle { .. } => false,
            },
            Self::Batch { priority } => match other {
                Self::Fifo { .. } | Self::RoundRobin { .. } | Self::Normal { .. } => true,
                Self::Batch { priority: other_priority } => *priority < other_priority,
                Self::Idle { .. } => false,
            },
            // see "the [...] nice value has no influence for [the SCHED_IDLE] policy" at sched(7).
            Self::Idle { .. } => match other {
                Self::Fifo { .. }
                | Self::RoundRobin { .. }
                | Self::Normal { .. }
                | Self::Batch { .. } => true,
                Self::Idle { .. } => false,
            },
        }
    }
}

pub fn min_priority_for_sched_policy(policy: u32) -> Result<u8, Errno> {
    match policy {
        SCHED_NORMAL | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => Ok(0),
        SCHED_FIFO | SCHED_RR => Ok(1),
        _ => error!(EINVAL),
    }
}

pub fn max_priority_for_sched_policy(policy: u32) -> Result<u8, Errno> {
    match policy {
        SCHED_NORMAL | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => Ok(0),
        SCHED_FIFO | SCHED_RR => Ok(99),
        _ => error!(EINVAL),
    }
}

/// Names of RoleManager roles for each static Zircon priority in the fair scheduler.
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
const REALTIME_ROLE_NAME: &str = "fuchsia.starnix.realtime";
// LINT.ThenChange(src/starnix/config/starnix.profiles)

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use starnix_uapi::errors::EINVAL;

    #[fuchsia::test]
    fn default_role_name() {
        assert_eq!(SchedulingPolicy::default().role_name(), "fuchsia.starnix.fair.16");
    }

    #[fuchsia::test]
    fn normal_with_non_default_nice_role_name() {
        assert_eq!(
            SchedulingPolicy::Normal { priority: 10 }.role_name(),
            "fuchsia.starnix.fair.11"
        );
        assert_eq!(
            SchedulingPolicy::Normal { priority: 27 }.role_name(),
            "fuchsia.starnix.fair.19"
        );
    }

    #[fuchsia::test]
    fn fifo_role_name() {
        assert_eq!(SchedulingPolicy::Fifo { priority: 1 }.role_name(), "fuchsia.starnix.realtime",);
        assert_eq!(SchedulingPolicy::Fifo { priority: 2 }.role_name(), "fuchsia.starnix.realtime",);
        assert_eq!(SchedulingPolicy::Fifo { priority: 99 }.role_name(), "fuchsia.starnix.realtime",);
    }

    #[fuchsia::test]
    fn idle_role_name() {
        assert_eq!(SchedulingPolicy::Idle { priority: 1 }.role_name(), "fuchsia.starnix.fair.0");
        assert_eq!(SchedulingPolicy::Idle { priority: 20 }.role_name(), "fuchsia.starnix.fair.0");
        assert_eq!(SchedulingPolicy::Idle { priority: 40 }.role_name(), "fuchsia.starnix.fair.0");
    }

    #[fuchsia::test]
    fn build_policy_from_sched_params() {
        assert_matches!(
            SchedulerState::from_sched_params(SCHED_NORMAL, sched_param { sched_priority: 0 }, 20),
            Ok(_)
        );
        assert_matches!(
            SchedulerState::from_sched_params(
                SCHED_NORMAL | SCHED_RESET_ON_FORK,
                sched_param { sched_priority: 0 },
                20
            ),
            Ok(_)
        );
        assert_matches!(
            SchedulerState::from_sched_params(
                SCHED_NORMAL,
                sched_param { sched_priority: 1 },
                20
            ),
            Err(e) if e == EINVAL
        );
        assert_matches!(
            SchedulerState::from_sched_params(SCHED_FIFO, sched_param { sched_priority: 1 }, 20),
            Ok(_)
        );
        assert_matches!(
            SchedulerState::from_sched_params(SCHED_FIFO, sched_param { sched_priority: 0 }, 20),
            Err(e) if e == EINVAL
        );
    }

    #[fuchsia::test]
    fn build_policy_from_binder() {
        assert_matches!(SchedulerState::from_binder(SCHED_NORMAL as u8, 0), Ok(_));
        assert_matches!(
            SchedulerState::from_binder(SCHED_NORMAL as u8, (((-21) as i8) as u8).into()),
            Err(_)
        );
        assert_matches!(
            SchedulerState::from_binder(SCHED_NORMAL as u8, (((-20) as i8) as u8).into()),
            Ok(_)
        );
        assert_matches!(SchedulerState::from_binder(SCHED_NORMAL as u8, 1), Ok(_));
        assert_matches!(SchedulerState::from_binder(SCHED_NORMAL as u8, 19), Ok(_));
        assert_matches!(SchedulerState::from_binder(SCHED_NORMAL as u8, 20), Err(_));
        assert_matches!(SchedulerState::from_binder(SCHED_FIFO as u8, 0), Err(_));
        assert_matches!(SchedulerState::from_binder(SCHED_FIFO as u8, 1), Ok(_));
        assert_matches!(SchedulerState::from_binder(SCHED_FIFO as u8, 99), Ok(_));
        assert_matches!(SchedulerState::from_binder(SCHED_FIFO as u8, 100), Err(_));
        assert_matches!(SchedulerState::from_binder(SCHED_RR as u8, 0), Err(_));
        assert_matches!(SchedulerState::from_binder(SCHED_RR as u8, 1), Ok(_));
        assert_matches!(SchedulerState::from_binder(SCHED_RR as u8, 99), Ok(_));
        assert_matches!(SchedulerState::from_binder(SCHED_RR as u8, 100), Err(_));
        assert_matches!(SchedulerState::from_binder(SCHED_BATCH as u8, 11), Ok(_));
        assert_matches!(SchedulerState::from_binder(42, 0), Err(_));
        assert_matches!(SchedulerState::from_binder(42, 0), Err(_));
    }

    // NOTE(https://fxbug.dev/425726327): some or all of this test may need to change based
    // on what is learned in https://fxbug.dev/425726327.
    #[fuchsia::test]
    fn is_less_than_for_binder() {
        let rr_50 = SchedulingPolicy::RoundRobin { priority: 50 };
        let rr_40 = SchedulingPolicy::RoundRobin { priority: 40 };
        let fifo_50 = SchedulingPolicy::Fifo { priority: 50 };
        let fifo_40 = SchedulingPolicy::Fifo { priority: 40 };
        let normal_40 = SchedulingPolicy::Normal { priority: 40 };
        let normal_10 = SchedulingPolicy::Normal { priority: 10 };
        let batch_40 = SchedulingPolicy::Batch { priority: 40 };
        let batch_30 = SchedulingPolicy::Batch { priority: 30 };
        let idle_40 = SchedulingPolicy::Idle { priority: 40 };
        let idle_30 = SchedulingPolicy::Idle { priority: 30 };
        assert!(!fifo_50.is_less_than_for_binder(fifo_50));
        assert!(!rr_50.is_less_than_for_binder(rr_50));
        assert!(!fifo_50.is_less_than_for_binder(rr_50));
        assert!(!rr_50.is_less_than_for_binder(fifo_50));
        assert!(!fifo_50.is_less_than_for_binder(rr_40));
        assert!(rr_40.is_less_than_for_binder(fifo_50));
        assert!(!rr_50.is_less_than_for_binder(fifo_40));
        assert!(fifo_40.is_less_than_for_binder(rr_50));
        assert!(!fifo_40.is_less_than_for_binder(normal_40));
        assert!(normal_40.is_less_than_for_binder(fifo_40));
        assert!(!rr_40.is_less_than_for_binder(normal_40));
        assert!(normal_40.is_less_than_for_binder(rr_40));
        assert!(!normal_40.is_less_than_for_binder(normal_40));
        assert!(!normal_40.is_less_than_for_binder(normal_10));
        assert!(normal_10.is_less_than_for_binder(normal_40));
        assert!(!normal_10.is_less_than_for_binder(batch_40));
        assert!(batch_40.is_less_than_for_binder(normal_10));
        assert!(!batch_40.is_less_than_for_binder(batch_40));
        assert!(!batch_40.is_less_than_for_binder(batch_30));
        assert!(batch_30.is_less_than_for_binder(batch_40));
        assert!(!batch_30.is_less_than_for_binder(idle_40));
        assert!(idle_40.is_less_than_for_binder(batch_30));
        assert!(!idle_40.is_less_than_for_binder(idle_40));
        assert!(!idle_40.is_less_than_for_binder(idle_30));
        assert!(!idle_30.is_less_than_for_binder(idle_40));
    }
}
