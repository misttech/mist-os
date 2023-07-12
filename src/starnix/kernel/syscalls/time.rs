// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, Task};

use crate::{mm::MemoryAccessorExt, syscalls::*, task::*, time::utc::*};

pub fn sys_clock_getres(
    current_task: &CurrentTask,
    which_clock: i32,
    tp_addr: UserRef<timespec>,
) -> Result<(), Errno> {
    if tp_addr.is_null() {
        return Ok(());
    }

    let tv = match which_clock as u32 {
        CLOCK_REALTIME
        | CLOCK_MONOTONIC
        | CLOCK_MONOTONIC_COARSE
        | CLOCK_MONOTONIC_RAW
        | CLOCK_BOOTTIME
        | CLOCK_THREAD_CPUTIME_ID
        | CLOCK_PROCESS_CPUTIME_ID => timespec { tv_sec: 0, tv_nsec: 1 },
        _ => {
            // Error if no dynamic clock can be found.
            let _ = get_dynamic_clock(current_task, which_clock)?;
            timespec { tv_sec: 0, tv_nsec: 1 }
        }
    };
    current_task.write_object(tp_addr, &tv)?;
    Ok(())
}

pub fn sys_clock_gettime(
    current_task: &CurrentTask,
    which_clock: i32,
    tp_addr: UserRef<timespec>,
) -> Result<(), Errno> {
    let nanos = if which_clock < 0 {
        get_dynamic_clock(current_task, which_clock)?
    } else {
        match which_clock as u32 {
            CLOCK_REALTIME | CLOCK_REALTIME_COARSE => utc_now().into_nanos(),
            CLOCK_MONOTONIC | CLOCK_MONOTONIC_COARSE | CLOCK_MONOTONIC_RAW | CLOCK_BOOTTIME => {
                zx::Time::get_monotonic().into_nanos()
            }
            CLOCK_THREAD_CPUTIME_ID => get_thread_cpu_time(current_task, current_task.id)?,
            CLOCK_PROCESS_CPUTIME_ID => get_process_cpu_time(current_task, current_task.id)?,
            _ => return error!(EINVAL),
        }
    };
    let tv = timespec { tv_sec: nanos / NANOS_PER_SECOND, tv_nsec: nanos % NANOS_PER_SECOND };
    current_task.write_object(tp_addr, &tv)?;
    Ok(())
}

pub fn sys_gettimeofday(
    current_task: &CurrentTask,
    user_tv: UserRef<timeval>,
    user_tz: UserRef<timezone>,
) -> Result<(), Errno> {
    if !user_tv.is_null() {
        let tv = timeval_from_time(utc_now());
        current_task.write_object(user_tv, &tv)?;
    }
    if !user_tz.is_null() {
        not_implemented!("gettimeofday does not implement tz argument");
    }
    Ok(())
}

pub fn sys_clock_nanosleep(
    current_task: &mut CurrentTask,
    which_clock: u32,
    flags: u32,
    user_request: UserRef<timespec>,
    user_remaining: UserRef<timespec>,
) -> Result<(), Errno> {
    let is_absolute = flags == TIMER_ABSTIME;
    // TODO(https://fxrev.dev/117507): For now, Starnix pretends that the monotonic and realtime
    // clocks advance at close to uniform rates and so we can treat relative realtime offsets the
    // same way that we treat relative monotonic clock offsets with a linear adjustment and retries
    // if we sleep for too little time.
    // At some point we'll need to monitor changes to the realtime clock proactively and adjust
    // timers accordingly.
    if which_clock != CLOCK_MONOTONIC && which_clock != CLOCK_REALTIME {
        not_implemented!("clock_nanosleep, clock {:?}, flags {:?}", which_clock, flags);
        return error!(EINVAL);
    }
    let request = current_task.read_object(user_request)?;
    log_trace!("clock_nanosleep({}, {}, {:?})", which_clock, flags, request);

    if timespec_is_zero(request) {
        return Ok(());
    }

    if which_clock == CLOCK_REALTIME {
        return clock_nanosleep_relative_to_utc(current_task, request, is_absolute, user_remaining);
    }

    let monotonic_deadline = if is_absolute {
        time_from_timespec(request)?
    } else {
        zx::Time::after(duration_from_timespec(request)?)
    };

    clock_nanosleep_monotonic_with_deadline(
        current_task,
        is_absolute,
        monotonic_deadline,
        None,
        user_remaining,
    )
}

// Sleep until we've satisfied |request| relative to the UTC clock which may advance at a different rate from the
// monotonic clock by repeatdly computing a monotonic target and sleeping.
fn clock_nanosleep_relative_to_utc(
    current_task: &mut CurrentTask,
    request: timespec,
    is_absolute: bool,
    user_remaining: UserRef<timespec>,
) -> Result<(), Errno> {
    let clock_deadline_absolute = if is_absolute {
        time_from_timespec(request)?
    } else {
        utc_now() + duration_from_timespec(request)?
    };
    loop {
        // Compute monotonic deadline that corresponds to the UTC clocks's current transformation to
        // monotonic. This may have changed while we were sleeping so check again on every
        // iteration.
        let monotonic_deadline =
            crate::time::utc::estimate_monotonic_deadline_from_utc(clock_deadline_absolute);
        clock_nanosleep_monotonic_with_deadline(
            current_task,
            is_absolute,
            monotonic_deadline,
            Some(clock_deadline_absolute),
            user_remaining,
        )?;
        // Look at |clock| again and decide if we're done.
        let clock_now = utc_now();
        if clock_now >= clock_deadline_absolute {
            return Ok(());
        }
        log_trace!(
            "clock_nanosleep_relative_to_clock short by {:?}, sleeping again",
            clock_deadline_absolute - clock_now
        );
    }
}

fn clock_nanosleep_monotonic_with_deadline(
    current_task: &mut CurrentTask,
    is_absolute: bool,
    deadline: zx::Time,
    original_utc_deadline: Option<zx::Time>,
    user_remaining: UserRef<timespec>,
) -> Result<(), Errno> {
    match Waiter::new().wait_until(current_task, deadline) {
        Err(err) if err == ETIMEDOUT => Ok(()),
        Err(err) if err == EINTR && is_absolute => error!(ERESTARTNOHAND),
        Err(err) if err == EINTR => {
            if !user_remaining.is_null() {
                let remaining = match original_utc_deadline {
                    Some(original_utc_deadline) => original_utc_deadline - utc_now(),
                    None => deadline - zx::Time::get_monotonic(),
                };
                let remaining =
                    timespec_from_duration(std::cmp::max(zx::Duration::from_nanos(0), remaining));
                current_task.write_object(user_remaining, &remaining)?;
            }
            current_task.set_syscall_restart_func(move |current_task| {
                clock_nanosleep_monotonic_with_deadline(
                    current_task,
                    is_absolute,
                    deadline,
                    original_utc_deadline,
                    user_remaining,
                )
            });
            error!(ERESTART_RESTARTBLOCK)
        }
        non_eintr => non_eintr,
    }
}

pub fn sys_nanosleep(
    current_task: &mut CurrentTask,
    user_request: UserRef<timespec>,
    user_remaining: UserRef<timespec>,
) -> Result<(), Errno> {
    sys_clock_nanosleep(current_task, CLOCK_REALTIME, 0, user_request, user_remaining)
}

/// Returns the cpu time for the task with the given `pid`.
///
/// Returns EINVAL if no such task can be found.
fn get_thread_cpu_time(current_task: &CurrentTask, pid: pid_t) -> Result<i64, Errno> {
    let task = current_task.get_task(pid).ok_or_else(|| errno!(EINVAL))?;
    let thread = task.thread.read();
    Ok(thread
        .as_ref()
        .ok_or_else(|| errno!(EINVAL))?
        .get_runtime_info()
        .map_err(|status| from_status_like_fdio!(status))?
        .cpu_time)
}

/// Returns the cpu time for the process associated with the given `pid`. `pid`
/// can be the `pid` for any task in the thread_group (so the caller can get the
/// process cpu time for any `task` by simply using `task.pid`).
///
/// Returns EINVAL if no such process can be found.
fn get_process_cpu_time(current_task: &CurrentTask, pid: pid_t) -> Result<i64, Errno> {
    let task = current_task.get_task(pid).ok_or_else(|| errno!(EINVAL))?;
    Ok(task
        .thread_group
        .process
        .get_runtime_info()
        .map_err(|status| from_status_like_fdio!(status))?
        .cpu_time)
}

/// Returns the type of cpu clock that `clock` encodes.
fn which_cpu_clock(clock: i32) -> i32 {
    const CPU_CLOCK_MASK: i32 = 3;
    clock & CPU_CLOCK_MASK
}

/// Returns whether or not `clock` encodes a valid clock type.
fn is_valid_cpu_clock(clock: i32) -> bool {
    const MAX_CPU_CLOCK: i32 = 3;
    if clock & 7 == 7 {
        return false;
    }
    if which_cpu_clock(clock) >= MAX_CPU_CLOCK {
        return false;
    }

    true
}

/// Returns the pid encoded in `clock`.
fn pid_of_clock_id(clock: i32) -> pid_t {
    // The pid is stored in the most significant 29 bits.
    !(clock >> 3) as pid_t
}

/// Returns true if the clock references a thread specific clock.
fn is_thread_clock(clock: i32) -> bool {
    const PER_THREAD_MASK: i32 = 4;
    clock & PER_THREAD_MASK != 0
}

/// Returns the cpu time for the clock specified in `which_clock`.
///
/// This is to support "dynamic clocks."
/// https://man7.org/linux/man-pages/man2/clock_gettime.2.html
///
/// `which_clock` is decoded as follows:
///   - Bit 0 and 1 are used to determine the type of clock.
///   - Bit 3 is used to determine whether the clock is for a thread or process.
///   - The remaining bits encode the pid of the thread/process.
fn get_dynamic_clock(current_task: &CurrentTask, which_clock: i32) -> Result<i64, Errno> {
    if !is_valid_cpu_clock(which_clock) {
        return error!(EINVAL);
    }

    let pid = pid_of_clock_id(which_clock);

    if is_thread_clock(which_clock) {
        get_thread_cpu_time(current_task, pid)
    } else {
        get_process_cpu_time(current_task, pid)
    }
}

pub fn sys_timer_create(
    current_task: &CurrentTask,
    clockid: uapi::__kernel_clockid_t,
    event: UserRef<sigevent>,
    timerid: UserRef<uapi::__kernel_timer_t>,
) -> Result<(), Errno> {
    not_implemented!("timer_create");
    let timers = &current_task.thread_group.read().timers;
    let user_event = current_task.read_object(event)?;
    let id = timers.create(clockid, &user_event)? as uapi::__kernel_timer_t;
    current_task.write_object(timerid, &id)?;
    Ok(())
}

pub fn sys_timer_delete(
    current_task: &CurrentTask,
    id: uapi::__kernel_timer_t,
) -> Result<(), Errno> {
    not_implemented!("timer_delete");
    let timers = &current_task.thread_group.read().timers;
    timers.delete(id as usize)
}

pub fn sys_timer_gettime(
    current_task: &CurrentTask,
    id: uapi::__kernel_timer_t,
    curr_value: UserRef<itimerspec>,
) -> Result<(), Errno> {
    let timers = &current_task.thread_group.read().timers;
    current_task.write_object(curr_value, &timers.get_time(id as usize)?)?;
    Ok(())
}

pub fn sys_timer_getoverrun(
    current_task: &CurrentTask,
    id: uapi::__kernel_timer_t,
) -> Result<i32, Errno> {
    let timers = &current_task.thread_group.read().timers;
    timers.get_overrun(id as usize)
}

pub fn sys_timer_settime(
    _current_task: &CurrentTask,
    _id: uapi::__kernel_timer_t,
    _flags: i32,
    _new_value: UserRef<itimerspec>,
    _old_value: UserRef<itimerspec>,
) -> Result<(), Errno> {
    not_implemented!("timer_settime");
    Ok(())
}

pub fn sys_getitimer(
    current_task: &CurrentTask,
    which: u32,
    user_curr_value: UserRef<itimerval>,
) -> Result<(), Errno> {
    let remaining = current_task.thread_group.get_itimer(which)?;
    current_task.write_object(user_curr_value, &remaining)?;
    Ok(())
}

pub fn sys_setitimer(
    current_task: &CurrentTask,
    which: u32,
    user_new_value: UserRef<itimerval>,
    user_old_value: UserRef<itimerval>,
) -> Result<(), Errno> {
    let new_value = current_task.read_object(user_new_value)?;

    let old_value = current_task.thread_group.set_itimer(which, new_value)?;

    if !user_old_value.is_null() {
        current_task.write_object(user_old_value, &old_value)?;
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{mm::PAGE_SIZE, testing::*};
    use fuchsia_zircon::HandleBased;

    #[::fuchsia::test]
    async fn test_nanosleep_without_remainder() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let task_clone = current_task.task_arc_clone();

        let thread = std::thread::spawn(move || {
            // Wait until the task is in nanosleep, and interrupt it.
            while !task_clone.read().signals.waiter.is_valid() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            task_clone.interrupt();
        });

        let duration = timespec_from_duration(zx::Duration::from_seconds(60));
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<timespec>() as u64,
        );
        current_task.write_object(address.into(), &duration).expect("write_object");

        // nanosleep will be interrupted by the current thread and should not fail with EFAULT
        // because the remainder pointer is null.
        assert_eq!(
            sys_nanosleep(&mut current_task, address.into(), UserRef::default()),
            error!(ERESTART_RESTARTBLOCK)
        );

        thread.join().expect("join");
    }

    #[::fuchsia::test]
    async fn test_clock_nanosleep_relative_to_slow_clock() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let test_clock = zx::Clock::create(zx::ClockOpts::AUTO_START, None).unwrap();
        let _test_clock_guard = UtcClockOverrideGuard::new(
            test_clock.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
        );

        // Slow |test_clock| down and verify that we sleep long enough.
        let slow_clock_update = zx::ClockUpdate::builder().rate_adjust(-1000).build();
        test_clock.update(slow_clock_update).unwrap();

        let before = test_clock.read().unwrap();

        let tv = timespec { tv_sec: 1, tv_nsec: 0 };

        let remaining = UserRef::new(UserAddress::default());

        super::clock_nanosleep_relative_to_utc(&mut current_task, tv, false, remaining).unwrap();
        let elapsed = test_clock.read().unwrap() - before;
        assert!(elapsed >= zx::Duration::from_seconds(1));
    }

    #[::fuchsia::test]
    async fn test_clock_nanosleep_interrupted_relative_to_fast_utc_clock() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let test_clock = zx::Clock::create(zx::ClockOpts::AUTO_START, None).unwrap();
        let _test_clock_guard = UtcClockOverrideGuard::new(
            test_clock.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
        );

        // Speed |test_clock| up.
        let slow_clock_update = zx::ClockUpdate::builder().rate_adjust(1000).build();
        test_clock.update(slow_clock_update).unwrap();

        let before = test_clock.read().unwrap();

        let tv = timespec { tv_sec: 2, tv_nsec: 0 };

        let remaining = {
            let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
            UserRef::new(addr)
        };

        // Interrupt the sleep roughly halfway through.
        let thread_group = std::sync::Arc::downgrade(&current_task.thread_group);
        let thread_join_handle = std::thread::Builder::new()
            .name("clock_nanosleep_interruptor".to_string())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if let Some(thread_group) = thread_group.upgrade() {
                    let signal_info = crate::signals::SignalInfo::default(signals::SIGALRM);
                    let signal_target =
                        thread_group.read().get_signal_target(&signal_info.signal.into());
                    if let Some(task) = &signal_target {
                        crate::signals::send_signal(task, signal_info);
                    }
                }
            })
            .unwrap();

        let result =
            super::clock_nanosleep_relative_to_utc(&mut current_task, tv, false, remaining);
        // We can't know deterministically if our interrupter thread will be able to interrupt our sleep.
        // If it did, result should be ERESTART_RESTARTBLOCK and |remaining| will be populated.
        // If it didn't, the result will be OK and |remaining| will not be touched.
        let mut remaining_written = Default::default();
        if result.is_err() {
            assert_eq!(result, error!(ERESTART_RESTARTBLOCK));
            remaining_written = {
                let mm = &current_task.mm;
                mm.read_object(remaining).unwrap()
            };
        }
        assert!(
            duration_from_timespec(remaining_written).unwrap() <= zx::Duration::from_seconds(1)
        );
        let elapsed = test_clock.read().unwrap() - before;
        thread_join_handle.join().unwrap();

        assert!(
            elapsed + duration_from_timespec(remaining_written).unwrap()
                >= zx::Duration::from_seconds(2)
        );
    }
}
