// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(clippy::wildcard_imports)]

use crate::arch::syscalls::sys_clone;
use crate::task::CurrentTask;
use fuchsia_inspect_contrib::profile_duration;
use paste::paste;
use starnix_sync::{Locked, Unlocked};
use starnix_syscalls::decls::Syscall;
use starnix_syscalls::SyscallResult;
use starnix_uapi::errors::Errno;

macro_rules! syscall_match_generic {
    {
        $path:path; $fn_prefix:ident; $locked:ident; $current_task:ident; $syscall_number:expr; $args:ident;
        $($(#[$match:meta])? $call:ident [$num_args:tt],)*
    } => {
        paste! {
            match $syscall_number as u32 {
                $(
                    $(#[$match])?
                    $path :: [<__NR_ $call>] => {
                        profile_duration!(stringify!($call));
                        match syscall_match_generic!(@call $locked; $current_task; $args; [<$fn_prefix $call>][$num_args]) {
                            Ok(x) => Ok(SyscallResult::from(x)),
                            Err(err) => Err(err),
                        }
                    },
                )*
                _ => sys_unknown($locked, $current_task, $syscall_number),
            }
        }
    };

    (@call $locked:ident; $current_task:ident; $args:ident; $func:ident [0]) => ($func($locked, $current_task));
    (@call $locked:ident; $current_task:ident; $args:ident; $func:ident [1]) => ($func($locked, $current_task, $args.0.into()));
    (@call $locked:ident; $current_task:ident; $args:ident; $func:ident [2]) => ($func($locked, $current_task, $args.0.into(), $args.1.into()));
    (@call $locked:ident; $current_task:ident; $args:ident; $func:ident [3]) => ($func($locked, $current_task, $args.0.into(), $args.1.into(), $args.2.into()));
    (@call $locked:ident; $current_task:ident; $args:ident; $func:ident [4]) => ($func($locked, $current_task, $args.0.into(), $args.1.into(), $args.2.into(), $args.3.into()));
    (@call $locked:ident; $current_task:ident; $args:ident; $func:ident [5]) => ($func($locked, $current_task, $args.0.into(), $args.1.into(), $args.2.into(), $args.3.into(), $args.4.into()));
    (@call $locked:ident; $current_task:ident; $args:ident; $func:ident [6]) => ($func($locked, $current_task, $args.0.into(), $args.1.into(), $args.2.into(), $args.3.into(), $args.4.into(), $args.5.into()));
}

macro_rules! syscall_match {
    {
        $($token:tt)*
    } => {
        syscall_match_generic! {
            starnix_uapi; sys_; $($token)*
        }
    }
}

#[cfg(all(target_arch = "aarch64", feature = "arch32"))]
macro_rules! arch32_syscall_match {
    {
        $($token:tt)*
    } => {
        syscall_match_generic! {
            starnix_uapi::arch32; sys_arch32_; $($token)*
        }
    }
}

pub fn dispatch_syscall(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    syscall: &Syscall,
) -> Result<SyscallResult, Errno> {
    use crate::bpf::syscalls::sys_bpf;
    use crate::mm::syscalls::{
        sys_brk, sys_futex, sys_get_robust_list, sys_madvise, sys_membarrier, sys_mincore,
        sys_mlock, sys_mlockall, sys_mmap, sys_mprotect, sys_mremap, sys_msync, sys_munlock,
        sys_munmap, sys_process_mrelease, sys_process_vm_readv, sys_process_vm_writev,
        sys_set_robust_list, sys_userfaultfd,
    };
    use crate::signals::syscalls::{
        sys_kill, sys_pidfd_send_signal, sys_restart_syscall, sys_rt_sigaction, sys_rt_sigpending,
        sys_rt_sigprocmask, sys_rt_sigqueueinfo, sys_rt_sigreturn, sys_rt_sigsuspend,
        sys_rt_sigtimedwait, sys_rt_tgsigqueueinfo, sys_sigaltstack, sys_signalfd4, sys_tgkill,
        sys_tkill, sys_wait4, sys_waitid,
    };
    use crate::syscalls::misc::{
        sys_delete_module, sys_getrandom, sys_perf_event_open, sys_personality, sys_reboot,
        sys_sched_yield, sys_setdomainname, sys_sethostname, sys_sysinfo, sys_uname, sys_unknown,
    };
    use crate::syscalls::time::{
        sys_clock_getres, sys_clock_gettime, sys_clock_nanosleep, sys_getitimer, sys_gettimeofday,
        sys_nanosleep, sys_setitimer, sys_settimeofday, sys_timer_create, sys_timer_delete,
        sys_timer_getoverrun, sys_timer_gettime, sys_timer_settime, sys_times,
    };
    use crate::task::syscalls::{
        sys_capget, sys_capset, sys_clone3, sys_execve, sys_execveat, sys_exit, sys_exit_group,
        sys_getcpu, sys_getegid, sys_geteuid, sys_getgid, sys_getgroups, sys_getpgid, sys_getpid,
        sys_getppid, sys_getpriority, sys_getresgid, sys_getresuid, sys_getrlimit, sys_getrusage,
        sys_getsid, sys_gettid, sys_getuid, sys_ioprio_set, sys_kcmp, sys_prctl, sys_prlimit64,
        sys_ptrace, sys_quotactl, sys_sched_get_priority_max, sys_sched_get_priority_min,
        sys_sched_getaffinity, sys_sched_getparam, sys_sched_getscheduler, sys_sched_setaffinity,
        sys_sched_setparam, sys_sched_setscheduler, sys_seccomp, sys_set_tid_address, sys_setfsgid,
        sys_setfsuid, sys_setgid, sys_setgroups, sys_setns, sys_setpgid, sys_setpriority,
        sys_setregid, sys_setresgid, sys_setresuid, sys_setreuid, sys_setrlimit, sys_setsid,
        sys_setuid, sys_swapoff, sys_swapon, sys_syslog, sys_unshare, sys_vhangup,
    };
    use crate::vfs::socket::syscalls::{
        sys_accept, sys_accept4, sys_bind, sys_connect, sys_getpeername, sys_getsockname,
        sys_getsockopt, sys_listen, sys_recvfrom, sys_recvmmsg, sys_recvmsg, sys_sendmmsg,
        sys_sendmsg, sys_sendto, sys_setsockopt, sys_shutdown, sys_socket, sys_socketpair,
    };
    use crate::vfs::syscalls::{
        sys_chdir, sys_chroot, sys_close, sys_close_range, sys_copy_file_range, sys_dup, sys_dup3,
        sys_epoll_create1, sys_epoll_ctl, sys_epoll_pwait, sys_epoll_pwait2, sys_eventfd2,
        sys_faccessat, sys_faccessat2, sys_fadvise64, sys_fallocate, sys_fchdir, sys_fchmod,
        sys_fchmodat, sys_fchown, sys_fchownat, sys_fcntl, sys_fdatasync, sys_fgetxattr,
        sys_flistxattr, sys_flock, sys_fremovexattr, sys_fsetxattr, sys_fstat, sys_fstatfs,
        sys_fsync, sys_ftruncate, sys_getcwd, sys_getdents64, sys_getxattr, sys_inotify_add_watch,
        sys_inotify_init1, sys_inotify_rm_watch, sys_io_cancel, sys_io_destroy, sys_io_getevents,
        sys_io_setup, sys_io_submit, sys_io_uring_enter, sys_io_uring_register, sys_io_uring_setup,
        sys_ioctl, sys_lgetxattr, sys_linkat, sys_listxattr, sys_llistxattr, sys_lremovexattr,
        sys_lseek, sys_lsetxattr, sys_memfd_create, sys_mkdirat, sys_mknodat, sys_mount,
        sys_newfstatat, sys_openat, sys_openat2, sys_pidfd_getfd, sys_pidfd_open, sys_pipe2,
        sys_ppoll, sys_pread64, sys_preadv, sys_preadv2, sys_pselect6, sys_pwrite64, sys_pwritev,
        sys_pwritev2, sys_read, sys_readahead, sys_readlinkat, sys_readv, sys_removexattr,
        sys_renameat2, sys_sendfile, sys_setxattr, sys_splice, sys_statfs, sys_statx,
        sys_symlinkat, sys_sync, sys_sync_file_range, sys_syncfs, sys_tee, sys_timerfd_create,
        sys_timerfd_gettime, sys_timerfd_settime, sys_truncate, sys_umask, sys_umount2,
        sys_unlinkat, sys_utimensat, sys_vmsplice, sys_write, sys_writev,
    };

    #[cfg(target_arch = "aarch64")]
    use crate::arch::syscalls::sys_renameat;

    #[cfg(all(target_arch = "aarch64", feature = "arch32"))]
    mod aarch64_arch32 {
        pub use crate::arch::syscalls::{sys_arch32_ARM_set_tls, sys_clone as sys_arch32_clone};
        pub use crate::mm::syscalls::{
            sys_arch32_mmap2, sys_arch32_munmap, sys_arch32_set_robust_list,
            sys_brk as sys_arch32_brk, sys_mprotect as sys_arch32_mprotect,
        };
        pub use crate::signals::syscalls::{
            sys_arch32_sigaltstack, sys_rt_sigaction as sys_arch32_rt_sigaction,
            sys_rt_sigprocmask as sys_arch32_rt_sigprocmask, sys_tgkill as sys_arch32_tgkill,
            sys_wait4 as sys_arch32_wait4,
        };
        pub use crate::syscalls::misc::{
            sys_arch32_uname, sys_getrandom as sys_arch32_getrandom,
            sys_personality as sys_arch32_personality,
        };
        pub use crate::syscalls::time::{
            sys_arch32_clock_getres, sys_arch32_clock_gettime, sys_arch32_gettimeofday,
            sys_clock_gettime as sys_arch32_clock_gettime64,
            sys_timer_gettime as sys_arch32_timer_gettime64,
        };
        pub use crate::task::syscalls::{
            sys_arch32_setrlimit, sys_arch32_ugetrlimit, sys_capget as sys_arch32_capget,
            sys_capset as sys_arch32_capset, sys_exit as sys_arch32_exit,
            sys_exit_group as sys_arch32_exit_group, sys_getpid as sys_arch32_getpid,
            sys_gettid as sys_arch32_gettid, sys_getuid as sys_arch32_getuid32,
            sys_prctl as sys_arch32_prctl, sys_prlimit64 as sys_arch32_prlimit64,
            sys_sched_getscheduler as sys_arch32_sched_getscheduler,
            sys_set_tid_address as sys_arch32_set_tid_address, sys_setuid as sys_arch32_setuid,
        };
        pub use crate::vfs::socket::syscalls::{
            sys_connect as sys_arch32_connect, sys_socket as sys_arch32_socket,
        };
        pub use crate::vfs::syscalls::{
            sys_arch32_access, sys_arch32_fstat64, sys_arch32_mkdir, sys_arch32_open,
            sys_arch32_readlink, sys_arch32_rmdir, sys_arch32_stat64,
            sys_close as sys_arch32_close, sys_faccessat as sys_arch32_faccessat,
            sys_fcntl as sys_arch32_fcntl64, sys_getcwd as sys_arch32_getcwd,
            sys_getdents64 as sys_arch32_getdents64, sys_ioctl as sys_arch32_ioctl,
            sys_lseek as sys_arch32_lseek, sys_memfd_create as sys_arch32_memfd_create,
            sys_newfstatat as sys_arch32_fstatat64, sys_openat as sys_arch32_openat,
            sys_pwritev as sys_arch32_pwritev, sys_read as sys_arch32_read,
            sys_readlinkat as sys_arch32_readlinkat, sys_umount2 as sys_arch32_umount2,
            sys_write as sys_arch32_write, sys_writev as sys_arch32_writev,
        };
    }
    #[cfg(all(target_arch = "aarch64", feature = "arch32"))]
    use aarch64_arch32::*;

    #[cfg(target_arch = "x86_64")]
    mod x86_64 {
        pub use crate::arch::syscalls::{
            sys_access, sys_alarm, sys_arch_prctl, sys_chmod, sys_chown, sys_creat, sys_dup2,
            sys_epoll_create, sys_epoll_wait, sys_eventfd, sys_fork, sys_getdents, sys_getpgrp,
            sys_inotify_init, sys_lchown, sys_link, sys_lstat, sys_mkdir, sys_mknod, sys_open,
            sys_pause, sys_pipe, sys_poll, sys_readlink, sys_rename, sys_renameat, sys_rmdir,
            sys_signalfd, sys_stat, sys_symlink, sys_time, sys_unlink, sys_vfork,
        };
        pub use crate::vfs::syscalls::sys_select;
    }
    #[cfg(target_arch = "x86_64")]
    use x86_64::*;

    let args = (syscall.arg0, syscall.arg1, syscall.arg2, syscall.arg3, syscall.arg4, syscall.arg5);

    #[cfg(all(target_arch = "aarch64", feature = "arch32"))]
    if current_task.thread_state.arch_width.is_arch32() {
        return arch32_syscall_match! {
            locked; current_task; syscall.decl.number; args;
            ARM_set_tls[1],
            access[2],
            brk[1],
            capget[2],
            capset[2],
            clock_getres[2],
            clock_gettime[2],
            clock_gettime64[2],
            clone[5],
            close[1],
            connect[3],
            exit[1],
            exit_group[1],
            faccessat[3],
            fcntl64[3],
            fstat64[2],
            fstatat64[4],
            getcwd[2],
            getdents64[3],
            getpid[0],
            getrandom[3],
            gettid[0],
            gettimeofday[2],
            getuid32[0],
            ioctl[3],
            lseek[3],
            memfd_create[2],
            mkdir[2],
            mmap2[6],
            mprotect[3],
            munmap[2],
            open[3],
            openat[4],
            personality[1],
            prctl[5],
            prlimit64[4],
            pwritev[4],
            read[3],
            readlink[3],
            readlinkat[4],
            rmdir[1],
            rt_sigaction[4],
            rt_sigprocmask[4],
            sched_getscheduler[1],
            setrlimit[2],
            set_robust_list[2],
            set_tid_address[1],
            setuid[1],
            sigaltstack[2],
            socket[3],
            stat64[2],
            tgkill[3],
            timer_gettime64[2],
            ugetrlimit[2],
            umount2[2],
            uname[1],
            wait4[4],
            write[3],
            writev[3],
        };
    }

    // An if-else isn't used to allow the above code to be removed and for
    // constant optimization to occur below.
    syscall_match! {
        locked; current_task; syscall.decl.number; args;
        accept4[4],
        accept[3],
        #[cfg(target_arch = "x86_64")] alarm[1],
        #[cfg(target_arch = "x86_64")] access[2],
        #[cfg(target_arch = "x86_64")] arch_prctl[2],
        bind[3],
        bpf[3],
        brk[1],
        capget[2],
        capset[2],
        chdir[1],
        #[cfg(target_arch = "x86_64")] chmod[2],
        #[cfg(target_arch = "x86_64")] chown[3],
        chroot[1],
        clock_getres[2],
        clock_gettime[2],
        clock_nanosleep[4],
        clone[5],
        clone3[2],
        close[1],
        close_range[3],
        connect[3],
        copy_file_range[6],
        #[cfg(target_arch = "x86_64")] creat[2],
        delete_module[2],
        #[cfg(target_arch = "x86_64")] dup2[2],
        dup3[3],
        dup[1],
        epoll_create1[1],
        #[cfg(target_arch = "x86_64")] epoll_create[1],
        epoll_ctl[4],
        epoll_pwait[5],
        epoll_pwait2[5],
        #[cfg(target_arch = "x86_64")] epoll_wait[4],
        eventfd2[2],
        #[cfg(target_arch = "x86_64")] eventfd[1],
        execve[3],
        execveat[5],
        exit[1],
        exit_group[1],
        faccessat2[4],
        faccessat[3],
        fadvise64[4],
        fallocate[4],
        fchdir[1],
        fchmod[2],
        fchmodat[3],
        fchown[3],
        fchownat[5],
        fcntl[3],
        fdatasync[1],
        fgetxattr[4],
        flistxattr[3],
        flock[2],
        #[cfg(target_arch = "x86_64")] fork[0],
        fremovexattr[2],
        fsetxattr[5],
        fstat[2],
        fstatfs[2],
        fsync[1],
        ftruncate[2],
        futex[6],
        get_robust_list[3],
        getcpu[2],
        getcwd[2],
        getdents64[3],
        #[cfg(target_arch = "x86_64")] getdents[3],
        getegid[0],
        geteuid[0],
        getgid[0],
        getgroups[2],
        getitimer[2],
        getpeername[3],
        getpgid[1],
        #[cfg(target_arch = "x86_64")] getpgrp[0],
        getpid[0],
        getppid[0],
        getpriority[2],
        getrandom[3],
        getresgid[3],
        getresuid[3],
        getrlimit[2],
        getrusage[2],
        getsid[1],
        getsockname[3],
        getsockopt[5],
        gettid[0],
        gettimeofday[2],
        getuid[0],
        getxattr[4],
        inotify_add_watch[3],
        inotify_init1[1],
        #[cfg(target_arch = "x86_64")] inotify_init[0],
        inotify_rm_watch[2],
        io_cancel[3],
        io_destroy[1],
        io_getevents[5],
        io_uring_setup[2],
        io_uring_enter[5],
        io_uring_register[4],
        io_setup[2],
        io_submit[3],
        ioctl[3],
        ioprio_set[3],
        kcmp[5],
        kill[2],
        #[cfg(target_arch = "x86_64")] lchown[3],
        lgetxattr[4],
        #[cfg(target_arch = "x86_64")] link[2],
        linkat[5],
        listen[2],
        listxattr[3],
        llistxattr[3],
        lremovexattr[2],
        lseek[3],
        lsetxattr[5],
        #[cfg(target_arch = "x86_64")] lstat[2],
        madvise[3],
        membarrier[3],
        memfd_create[2],
        mincore[3],
        #[cfg(target_arch = "x86_64")] mkdir[2],
        mkdirat[3],
        #[cfg(target_arch = "x86_64")] mknod[3],
        mknodat[4],
        mlock[2],
        mlockall[1],
        mmap[6],
        mount[5],
        mprotect[3],
        mremap[5],
        msync[3],
        munlock[2],
        munmap[2],
        nanosleep[2],
        newfstatat[4],
        #[cfg(target_arch = "x86_64")] open[3],
        openat[4],
        openat2[4],
        perf_event_open[5],
        personality[1],
        pidfd_getfd[3],
        pidfd_open[2],
        pidfd_send_signal[4],
        #[cfg(target_arch = "x86_64")] pause[0],
        pipe2[2],
        #[cfg(target_arch = "x86_64")] pipe[1],
        #[cfg(target_arch = "x86_64")] poll[3],
        ppoll[5],
        prctl[5],
        pread64[4],
        preadv[4],
        preadv2[6],
        prlimit64[4],
        process_mrelease[2],
        process_vm_readv[6],
        process_vm_writev[6],
        pselect6[6],
        ptrace[4],
        pwrite64[4],
        pwritev[4],
        pwritev2[6],
        quotactl[4],
        read[3],
        readahead[3],
        #[cfg(target_arch = "x86_64")] readlink[3],
        readlinkat[4],
        readv[3],
        reboot[4],
        recvfrom[6],
        recvmmsg[5],
        recvmsg[3],
        removexattr[2],
        #[cfg(target_arch = "x86_64")] rename[2],
        renameat2[5],
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))] renameat[4],
        restart_syscall[0],
        #[cfg(target_arch = "x86_64")] rmdir[1],
        rt_sigaction[4],
        rt_sigpending[2],
        rt_sigprocmask[4],
        rt_sigqueueinfo[3],
        rt_sigreturn[0],
        rt_sigsuspend[2],
        rt_sigtimedwait[4],
        rt_tgsigqueueinfo[4],
        sched_get_priority_min[1],
        sched_get_priority_max[1],
        sched_getaffinity[3],
        sched_getparam[2],
        sched_getscheduler[1],
        sched_setaffinity[3],
        sched_setscheduler[3],
        sched_setparam[2],
        sched_yield[0],
        seccomp[3],
        #[cfg(target_arch = "x86_64")] select[5],
        sendmmsg[4],
        sendmsg[3],
        sendto[6],
        sendfile[4],
        set_robust_list[2],
        set_tid_address[1],
        setdomainname[2],
        setfsgid[1],
        setfsuid[1],
        setgid[1],
        setgroups[2],
        sethostname[2],
        setitimer[3],
        setns[2],
        setpgid[2],
        setpriority[3],
        setresgid[3],
        setresuid[3],
        setregid[2],
        setreuid[2],
        setrlimit[2],
        setsid[0],
        setsockopt[5],
        settimeofday[2],
        setuid[1],
        setxattr[5],
        shutdown[2],
        sigaltstack[2],
        #[cfg(target_arch = "x86_64")] signalfd[3],
        signalfd4[4],
        socket[3],
        socketpair[4],
        splice[6],
        #[cfg(target_arch = "x86_64")] stat[2],
        statfs[2],
        statx[5],
        swapoff[1],
        swapon[2],
        #[cfg(target_arch = "x86_64")] symlink[2],
        symlinkat[3],
        sync_file_range[4],
        sync[0],
        syncfs[1],
        sysinfo[1],
        syslog[3],
        tee[4],
        tgkill[3],
        #[cfg(target_arch = "x86_64")] time[1],
        timer_create[3],
        timer_delete[1],
        timer_gettime[2],
        timer_getoverrun[1],
        timer_settime[4],
        timerfd_create[2],
        timerfd_gettime[2],
        timerfd_settime[4],
        times[1],
        tkill[2],
        truncate[2],
        umask[1],
        umount2[2],
        uname[1],
        #[cfg(target_arch = "x86_64")] unlink[1],
        unlinkat[3],
        unshare[1],
        userfaultfd[1],
        utimensat[4],
        vhangup[0],
        #[cfg(target_arch = "x86_64")] vfork[0],
        vmsplice[4],
        wait4[4],
        waitid[5],
        write[3],
        writev[3],
    }
}
