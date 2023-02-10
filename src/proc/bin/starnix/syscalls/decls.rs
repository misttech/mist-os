// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use fuchsia_inspect as inspect;
use lazy_static::lazy_static;
use paste::paste;

use crate::types::*;

/// Helper for for_each_syscall! that adds any architecture-specific syscalls.
///
/// X86_64 has many unique syscalls for legacy reasons. Newer architectures funnel some of these
/// through some newer and more general variants. The variants used by other platforms are listed in
/// the comments below.
#[cfg(target_arch = "x86_64")]
macro_rules! for_each_arch_syscall {
    {$callback:ident; $($context:ident;)* ; $($common_name:ident,)*} => {
        $callback!{
            $($context;)*
            $($common_name,)*
            access,  // faccessat
            afs_syscall, // (deprecated)
            alarm,  // setitimer
            arch_prctl,  // (unused)
            chmod,  // fchmodat
            chown,  // fchownat
            create_module, // (deprecated)
            creat,  // openat
            dup2,  // dup3
            epoll_create,  // epoll_create1
            epoll_ctl_old,  // (unused)
            epoll_wait,  // epoll_pwait
            epoll_wait_old,  // (unused)
            eventfd,  // eventfd2
            fork,  // clone
            futimesat,  // (deprecated)
            getdents,  // getdents64
            get_kernel_syms, // (deprecated)
            getpgrp,  // getpgid
            getpmsg, // (unused)
            get_thread_area,  // (unused)
            inotify_init,  // inotify_init1
            ioperm,  // (unused)
            iopl,  // (deprevated)
            lchown,  // fchownat
            link,  // linkat
            lstat,  // fstatat
            mkdir,  // mkdirat
            mknod,  // mknodat
            modify_ldt,  // (unused)
            open,  // openat
            pause,  // sigsuspend
            pipe,  // pipe2
            poll,  // ppoll
            putpmsg, // (unused)
            query_module, // (deprecated)
            readlink,  // readlinkat
            rename,  // renameat
            rmdir,  // unlinkat
            security,  // (unused)
            select,  // pselect
            set_thread_area, // (unused)
            signalfd,  // signalfd4
            stat,  // fstatat
            symlink,  // symlinkat
            _sysctl,  // (deprecated)
            sysfs,  // (deprecated)
            time,  // gettimeofday
            tuxcall,  // (unused)
            unlink,  // unlinkat
            uselib,  // (deprecated)
            ustat,  // (deprecated)
            utimes,  // utimesat
            utime,  // utimesat
            vfork,  // clone
            vserver,  // (unused)
        }
    }
}

#[cfg(target_arch = "aarch64")]
macro_rules! for_each_arch_syscall {
    {$callback:ident; $($context:ident;)* ; $($common_name:ident,)*} => {
        $callback!{
            $($context;)*
            $($common_name,)*
        }
    }
}

/// Intended to be used with other macros to produce code that needs to handle
/// each syscall.
///
/// This list contains all cross-architecture syscalls, and delegates through for_each_arch_syscall!
/// to add in any architecture-specific ones.
macro_rules! for_each_syscall {
    {$callback:ident $(,$context:ident)*} => {
        for_each_arch_syscall!{
            $callback;
            $($context;)*
            ;
            read,
            write,
            close,
            fstat,
            lseek,
            mmap,
            mprotect,
            munmap,
            brk,
            rt_sigaction,
            rt_sigprocmask,
            rt_sigreturn,
            ioctl,
            pread64,
            pwrite64,
            readv,
            writev,
            sched_yield,
            mremap,
            msync,
            mincore,
            madvise,
            shmget,
            shmat,
            shmctl,
            dup,
            nanosleep,
            getitimer,
            setitimer,
            getpid,
            sendfile,
            socket,
            connect,
            accept,
            sendto,
            recvfrom,
            sendmsg,
            recvmsg,
            shutdown,
            bind,
            listen,
            getsockname,
            getpeername,
            socketpair,
            setsockopt,
            getsockopt,
            clone,
            execve,
            exit,
            wait4,
            kill,
            uname,
            semget,
            semop,
            semctl,
            shmdt,
            msgget,
            msgsnd,
            msgrcv,
            msgctl,
            fcntl,
            flock,
            fsync,
            fdatasync,
            truncate,
            ftruncate,
            getcwd,
            chdir,
            fchdir,
            fchmod,
            fchown,
            umask,
            gettimeofday,
            getrlimit,
            getrusage,
            sysinfo,
            times,
            ptrace,
            getuid,
            syslog,
            getgid,
            setuid,
            setgid,
            geteuid,
            getegid,
            setpgid,
            getppid,
            setsid,
            setreuid,
            setregid,
            getgroups,
            setgroups,
            setresuid,
            getresuid,
            setresgid,
            getresgid,
            getpgid,
            setfsuid,
            setfsgid,
            getsid,
            capget,
            capset,
            rt_sigpending,
            rt_sigtimedwait,
            rt_sigqueueinfo,
            rt_sigsuspend,
            sigaltstack,
            personality,
            statfs,
            fstatfs,
            getpriority,
            setpriority,
            sched_setparam,
            sched_getparam,
            sched_setscheduler,
            sched_getscheduler,
            sched_get_priority_max,
            sched_get_priority_min,
            sched_rr_get_interval,
            mlock,
            munlock,
            mlockall,
            munlockall,
            vhangup,
            pivot_root,
            prctl,
            adjtimex,
            setrlimit,
            chroot,
            sync,
            acct,
            settimeofday,
            mount,
            umount2,
            swapon,
            swapoff,
            reboot,
            sethostname,
            setdomainname,
            init_module,
            delete_module,
            quotactl,
            nfsservctl,
            gettid,
            readahead,
            setxattr,
            lsetxattr,
            fsetxattr,
            getxattr,
            lgetxattr,
            fgetxattr,
            listxattr,
            llistxattr,
            flistxattr,
            removexattr,
            lremovexattr,
            fremovexattr,
            tkill,
            futex,
            sched_setaffinity,
            sched_getaffinity,
            io_setup,
            io_destroy,
            io_getevents,
            io_submit,
            io_cancel,
            lookup_dcookie,
            remap_file_pages,
            getdents64,
            set_tid_address,
            restart_syscall,
            semtimedop,
            fadvise64,
            timer_create,
            timer_settime,
            timer_gettime,
            timer_getoverrun,
            timer_delete,
            clock_settime,
            clock_gettime,
            clock_getres,
            clock_nanosleep,
            exit_group,
            epoll_ctl,
            tgkill,
            mbind,
            set_mempolicy,
            get_mempolicy,
            mq_open,
            mq_unlink,
            mq_timedsend,
            mq_timedreceive,
            mq_notify,
            mq_getsetattr,
            kexec_load,
            waitid,
            add_key,
            request_key,
            keyctl,
            ioprio_set,
            ioprio_get,
            inotify_add_watch,
            inotify_rm_watch,
            migrate_pages,
            openat,
            mkdirat,
            mknodat,
            fchownat,
            newfstatat,
            unlinkat,
            renameat,
            linkat,
            symlinkat,
            readlinkat,
            fchmodat,
            faccessat,
            pselect6,
            ppoll,
            unshare,
            set_robust_list,
            get_robust_list,
            splice,
            tee,
            sync_file_range,
            vmsplice,
            move_pages,
            utimensat,
            epoll_pwait,
            timerfd_create,
            fallocate,
            timerfd_settime,
            timerfd_gettime,
            accept4,
            signalfd4,
            eventfd2,
            epoll_create1,
            dup3,
            pipe2,
            inotify_init1,
            preadv,
            pwritev,
            rt_tgsigqueueinfo,
            perf_event_open,
            recvmmsg,
            fanotify_init,
            fanotify_mark,
            prlimit64,
            name_to_handle_at,
            open_by_handle_at,
            clock_adjtime,
            syncfs,
            sendmmsg,
            setns,
            getcpu,
            process_vm_readv,
            process_vm_writev,
            kcmp,
            finit_module,
            sched_setattr,
            sched_getattr,
            renameat2,
            seccomp,
            getrandom,
            memfd_create,
            kexec_file_load,
            bpf,
            execveat,
            userfaultfd,
            membarrier,
            mlock2,
            copy_file_range,
            preadv2,
            pwritev2,
            pkey_mprotect,
            pkey_alloc,
            pkey_free,
            statx,
            io_pgetevents,
            rseq,
            pidfd_send_signal,
            io_uring_setup,
            io_uring_enter,
            io_uring_register,
            open_tree,
            move_mount,
            fsopen,
            fsconfig,
            fsmount,
            fspick,
            pidfd_open,
            clone3,
            close_range,
            openat2,
            pidfd_getfd,
            faccessat2,
            process_madvise,
            epoll_pwait2,
        }
    }
}

/// A system call declaration.
///
/// Describes the name of the syscall and its number.
pub struct SyscallDecl {
    pub name: &'static str,
    pub number: u64,
}

impl std::fmt::Debug for SyscallDecl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.name, self.number,)
    }
}

/// A particular invocation of a system call.
///
/// Contains the declaration of the invoked system call, as well as which arguments it was invoked
/// with.
pub struct Syscall {
    pub decl: &'static SyscallDecl,
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
    pub arg4: u64,
    pub arg5: u64,
}

impl std::fmt::Debug for Syscall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?}({:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x})",
            self.decl, self.arg0, self.arg1, self.arg2, self.arg3, self.arg4, self.arg5
        )
    }
}

/// A macro for declaring a const SyscallDecl for a given syscall.
///
/// The constant will be called DECL_<SYSCALL>.
macro_rules! syscall_decl {
    {$($name:ident,)*} => {
        paste! {
            $(pub const [<DECL_ $name:upper>]: SyscallDecl = SyscallDecl { name: stringify!($name), number: [<__NR_ $name>] as u64};)*
        }
    }
}

// Produce each syscall declaration.
for_each_syscall! {syscall_decl}

/// A macro for declaring a SyscallDecl stats property.
macro_rules! syscall_stats_property {
    ($($name:ident,)*) => {
        paste!{
            $(
                lazy_static!{
                static ref [<SYSCALL_ $name:upper _STATS>]: inspect::UintProperty =
                    SYSCALL_STATS_NODE.create_uint(stringify!($name), 0);
                }
            )*
        }
    }
}

lazy_static! {
    static ref SYSCALL_STATS_NODE: inspect::Node =
        inspect::component::inspector().root().create_child("syscall_stats");
    static ref SYSCALL_UNKNOWN_STATS: inspect::UintProperty =
        SYSCALL_STATS_NODE.create_uint("<unknown>", 0);
}

// Produce each syscall stats property.
for_each_syscall! {syscall_stats_property}

/// A declaration for an unknown syscall.
///
/// Useful so that functions that return a SyscallDecl have a sentinel
/// to return when they cannot find an appropriate syscall.
pub const DECL_UNKNOWN: SyscallDecl = SyscallDecl { name: "<unknown>", number: 0xFFFF };

/// A macro for the body of SyscallDecl::from_number.
///
/// Evaluates to the &'static SyscallDecl for the given number or to
/// &DECL_UNKNOWN if the number is unknown.
macro_rules! syscall_match {
    {$number:ident; $($name:ident,)*} => {
        paste! {
            match $number as u32 {
                $([<__NR_ $name>] => &[<DECL_ $name:upper>],)*
                _ => &DECL_UNKNOWN,
            }
        }
    }
}

#[cfg(feature = "syscall_stats")]
macro_rules! syscall_match_stats {
    {$number:ident; $($name:ident,)*} => {
        paste! {
            match $number as u32 {
                $([<__NR_ $name>] => &[<SYSCALL_ $name:upper _STATS>],)*
                _ => &SYSCALL_UNKNOWN_STATS,
            }
        }
    }
}

impl SyscallDecl {
    /// The SyscallDecl for the given syscall number.
    ///
    /// Returns &DECL_UNKNOWN if the given syscall number is not known.
    pub fn from_number(number: u64) -> &'static SyscallDecl {
        for_each_syscall! { syscall_match, number }
    }

    #[cfg(feature = "syscall_stats")]
    pub fn stats_property(number: u64) -> &'static inspect::UintProperty {
        for_each_syscall! { syscall_match_stats, number }
    }
}
