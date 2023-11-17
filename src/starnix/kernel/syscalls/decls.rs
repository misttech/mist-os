// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use paste::paste;

use crate::syscalls::SyscallArg;
use starnix_uapi::{
    __NR_accept, __NR_accept4, __NR_acct, __NR_add_key, __NR_adjtimex, __NR_bind, __NR_bpf,
    __NR_brk, __NR_capget, __NR_capset, __NR_chdir, __NR_chroot, __NR_clock_adjtime,
    __NR_clock_getres, __NR_clock_gettime, __NR_clock_nanosleep, __NR_clock_settime, __NR_clone,
    __NR_clone3, __NR_close, __NR_close_range, __NR_connect, __NR_copy_file_range,
    __NR_delete_module, __NR_dup, __NR_dup3, __NR_epoll_create1, __NR_epoll_ctl, __NR_epoll_pwait,
    __NR_epoll_pwait2, __NR_eventfd2, __NR_execve, __NR_execveat, __NR_exit, __NR_exit_group,
    __NR_faccessat, __NR_faccessat2, __NR_fadvise64, __NR_fallocate, __NR_fanotify_init,
    __NR_fanotify_mark, __NR_fchdir, __NR_fchmod, __NR_fchmodat, __NR_fchown, __NR_fchownat,
    __NR_fcntl, __NR_fdatasync, __NR_fgetxattr, __NR_finit_module, __NR_flistxattr, __NR_flock,
    __NR_fremovexattr, __NR_fsconfig, __NR_fsetxattr, __NR_fsmount, __NR_fsopen, __NR_fspick,
    __NR_fstat, __NR_fstatfs, __NR_fsync, __NR_ftruncate, __NR_futex, __NR_get_mempolicy,
    __NR_get_robust_list, __NR_getcpu, __NR_getcwd, __NR_getdents64, __NR_getegid, __NR_geteuid,
    __NR_getgid, __NR_getgroups, __NR_getitimer, __NR_getpeername, __NR_getpgid, __NR_getpid,
    __NR_getppid, __NR_getpriority, __NR_getrandom, __NR_getresgid, __NR_getresuid, __NR_getrlimit,
    __NR_getrusage, __NR_getsid, __NR_getsockname, __NR_getsockopt, __NR_gettid, __NR_gettimeofday,
    __NR_getuid, __NR_getxattr, __NR_init_module, __NR_inotify_add_watch, __NR_inotify_init1,
    __NR_inotify_rm_watch, __NR_io_cancel, __NR_io_destroy, __NR_io_getevents, __NR_io_pgetevents,
    __NR_io_setup, __NR_io_submit, __NR_io_uring_enter, __NR_io_uring_register,
    __NR_io_uring_setup, __NR_ioctl, __NR_ioprio_get, __NR_ioprio_set, __NR_kcmp,
    __NR_kexec_file_load, __NR_kexec_load, __NR_keyctl, __NR_kill, __NR_lgetxattr, __NR_linkat,
    __NR_listen, __NR_listxattr, __NR_llistxattr, __NR_lookup_dcookie, __NR_lremovexattr,
    __NR_lseek, __NR_lsetxattr, __NR_madvise, __NR_mbind, __NR_membarrier, __NR_memfd_create,
    __NR_migrate_pages, __NR_mincore, __NR_mkdirat, __NR_mknodat, __NR_mlock, __NR_mlock2,
    __NR_mlockall, __NR_mmap, __NR_mount, __NR_move_mount, __NR_move_pages, __NR_mprotect,
    __NR_mq_getsetattr, __NR_mq_notify, __NR_mq_open, __NR_mq_timedreceive, __NR_mq_timedsend,
    __NR_mq_unlink, __NR_mremap, __NR_msgctl, __NR_msgget, __NR_msgrcv, __NR_msgsnd, __NR_msync,
    __NR_munlock, __NR_munlockall, __NR_munmap, __NR_name_to_handle_at, __NR_nanosleep,
    __NR_newfstatat, __NR_nfsservctl, __NR_open_by_handle_at, __NR_open_tree, __NR_openat,
    __NR_openat2, __NR_perf_event_open, __NR_personality, __NR_pidfd_getfd, __NR_pidfd_open,
    __NR_pidfd_send_signal, __NR_pipe2, __NR_pivot_root, __NR_pkey_alloc, __NR_pkey_free,
    __NR_pkey_mprotect, __NR_ppoll, __NR_prctl, __NR_pread64, __NR_preadv, __NR_preadv2,
    __NR_prlimit64, __NR_process_madvise, __NR_process_vm_readv, __NR_process_vm_writev,
    __NR_pselect6, __NR_ptrace, __NR_pwrite64, __NR_pwritev, __NR_pwritev2, __NR_quotactl,
    __NR_read, __NR_readahead, __NR_readlinkat, __NR_readv, __NR_reboot, __NR_recvfrom,
    __NR_recvmmsg, __NR_recvmsg, __NR_remap_file_pages, __NR_removexattr, __NR_renameat2,
    __NR_request_key, __NR_restart_syscall, __NR_rseq, __NR_rt_sigaction, __NR_rt_sigpending,
    __NR_rt_sigprocmask, __NR_rt_sigqueueinfo, __NR_rt_sigreturn, __NR_rt_sigsuspend,
    __NR_rt_sigtimedwait, __NR_rt_tgsigqueueinfo, __NR_sched_get_priority_max,
    __NR_sched_get_priority_min, __NR_sched_getaffinity, __NR_sched_getattr, __NR_sched_getparam,
    __NR_sched_getscheduler, __NR_sched_rr_get_interval, __NR_sched_setaffinity,
    __NR_sched_setattr, __NR_sched_setparam, __NR_sched_setscheduler, __NR_sched_yield,
    __NR_seccomp, __NR_semctl, __NR_semget, __NR_semop, __NR_semtimedop, __NR_sendfile,
    __NR_sendmmsg, __NR_sendmsg, __NR_sendto, __NR_set_mempolicy, __NR_set_robust_list,
    __NR_set_tid_address, __NR_setdomainname, __NR_setfsgid, __NR_setfsuid, __NR_setgid,
    __NR_setgroups, __NR_sethostname, __NR_setitimer, __NR_setns, __NR_setpgid, __NR_setpriority,
    __NR_setregid, __NR_setresgid, __NR_setresuid, __NR_setreuid, __NR_setrlimit, __NR_setsid,
    __NR_setsockopt, __NR_settimeofday, __NR_setuid, __NR_setxattr, __NR_shmat, __NR_shmctl,
    __NR_shmdt, __NR_shmget, __NR_shutdown, __NR_sigaltstack, __NR_signalfd4, __NR_socket,
    __NR_socketpair, __NR_splice, __NR_statfs, __NR_statx, __NR_swapoff, __NR_swapon,
    __NR_symlinkat, __NR_sync, __NR_sync_file_range, __NR_syncfs, __NR_sysinfo, __NR_syslog,
    __NR_tee, __NR_tgkill, __NR_timer_create, __NR_timer_delete, __NR_timer_getoverrun,
    __NR_timer_gettime, __NR_timer_settime, __NR_timerfd_create, __NR_timerfd_gettime,
    __NR_timerfd_settime, __NR_times, __NR_tkill, __NR_truncate, __NR_umask, __NR_umount2,
    __NR_uname, __NR_unlinkat, __NR_unshare, __NR_userfaultfd, __NR_utimensat, __NR_vhangup,
    __NR_vmsplice, __NR_wait4, __NR_waitid, __NR_write, __NR_writev,
};

#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR__sysctl;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_access;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_afs_syscall;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_alarm;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_arch_prctl;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_chmod;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_chown;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_creat;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_create_module;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_dup2;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_epoll_create;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_epoll_ctl_old;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_epoll_wait;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_epoll_wait_old;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_eventfd;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_fork;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_futimesat;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_get_kernel_syms;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_get_thread_area;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_getdents;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_getpgrp;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_getpmsg;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_inotify_init;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_ioperm;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_iopl;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_lchown;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_link;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_lstat;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_mkdir;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_mknod;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_modify_ldt;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_open;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_pause;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_pipe;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_poll;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_putpmsg;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_query_module;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_readlink;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_rename;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_renameat;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_rmdir;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_security;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_select;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_set_thread_area;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_signalfd;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_stat;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_symlink;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_sysfs;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_time;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_tuxcall;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_unlink;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_uselib;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_ustat;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_utime;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_utimes;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_vfork;
#[cfg(target_arch = "x86_64")]
use starnix_uapi::__NR_vserver;

#[cfg(target_arch = "aarch64")]
use starnix_uapi::__NR_renameat;

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
            rename,  // renameat2
            renameat,  // renameat2
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
            renameat,  // renameat2
        }
    }
}

#[cfg(target_arch = "riscv64")]
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
            accept,
            accept4,
            acct,
            add_key,
            adjtimex,
            bind,
            bpf,
            brk,
            capget,
            capset,
            chdir,
            chroot,
            clock_adjtime,
            clock_getres,
            clock_gettime,
            clock_nanosleep,
            clock_settime,
            clone,
            clone3,
            close,
            close_range,
            connect,
            copy_file_range,
            delete_module,
            dup,
            dup3,
            epoll_create1,
            epoll_ctl,
            epoll_pwait,
            epoll_pwait2,
            eventfd2,
            execve,
            execveat,
            exit,
            exit_group,
            faccessat,
            faccessat2,
            fadvise64,
            fallocate,
            fanotify_init,
            fanotify_mark,
            fchdir,
            fchmod,
            fchmodat,
            fchown,
            fchownat,
            fcntl,
            fdatasync,
            fgetxattr,
            finit_module,
            flistxattr,
            flock,
            fremovexattr,
            fsconfig,
            fsetxattr,
            fsmount,
            fsopen,
            fspick,
            fstat,
            fstatfs,
            fsync,
            ftruncate,
            futex,
            getcpu,
            getcwd,
            getdents64,
            getegid,
            geteuid,
            getgid,
            getgroups,
            getitimer,
            get_mempolicy,
            getpeername,
            getpgid,
            getpid,
            getppid,
            getpriority,
            getrandom,
            getresgid,
            getresuid,
            getrlimit,
            get_robust_list,
            getrusage,
            getsid,
            getsockname,
            getsockopt,
            gettid,
            gettimeofday,
            getuid,
            getxattr,
            init_module,
            inotify_add_watch,
            inotify_init1,
            inotify_rm_watch,
            io_cancel,
            ioctl,
            io_destroy,
            io_getevents,
            io_pgetevents,
            ioprio_get,
            ioprio_set,
            io_setup,
            io_submit,
            io_uring_enter,
            io_uring_register,
            io_uring_setup,
            kcmp,
            kexec_file_load,
            kexec_load,
            keyctl,
            kill,
            lgetxattr,
            linkat,
            listen,
            listxattr,
            llistxattr,
            lookup_dcookie,
            lremovexattr,
            lseek,
            lsetxattr,
            madvise,
            mbind,
            membarrier,
            memfd_create,
            migrate_pages,
            mincore,
            mkdirat,
            mknodat,
            mlock,
            mlock2,
            mlockall,
            mmap,
            mount,
            move_mount,
            move_pages,
            mprotect,
            mq_getsetattr,
            mq_notify,
            mq_open,
            mq_timedreceive,
            mq_timedsend,
            mq_unlink,
            mremap,
            msgctl,
            msgget,
            msgrcv,
            msgsnd,
            msync,
            munlock,
            munlockall,
            munmap,
            name_to_handle_at,
            nanosleep,
            newfstatat,
            nfsservctl,
            openat,
            openat2,
            open_by_handle_at,
            open_tree,
            perf_event_open,
            personality,
            pidfd_getfd,
            pidfd_open,
            pidfd_send_signal,
            pipe2,
            pivot_root,
            pkey_alloc,
            pkey_free,
            pkey_mprotect,
            ppoll,
            prctl,
            pread64,
            preadv,
            preadv2,
            prlimit64,
            process_madvise,
            process_vm_readv,
            process_vm_writev,
            pselect6,
            ptrace,
            pwrite64,
            pwritev,
            pwritev2,
            quotactl,
            read,
            readahead,
            readlinkat,
            readv,
            reboot,
            recvfrom,
            recvmmsg,
            recvmsg,
            remap_file_pages,
            removexattr,
            renameat2,
            request_key,
            restart_syscall,
            rseq,
            rt_sigaction,
            rt_sigpending,
            rt_sigprocmask,
            rt_sigqueueinfo,
            rt_sigreturn,
            rt_sigsuspend,
            rt_sigtimedwait,
            rt_tgsigqueueinfo,
            sched_getaffinity,
            sched_getattr,
            sched_getparam,
            sched_get_priority_max,
            sched_get_priority_min,
            sched_getscheduler,
            sched_rr_get_interval,
            sched_setaffinity,
            sched_setattr,
            sched_setparam,
            sched_setscheduler,
            sched_yield,
            seccomp,
            semctl,
            semget,
            semop,
            semtimedop,
            sendfile,
            sendmmsg,
            sendmsg,
            sendto,
            setdomainname,
            setfsgid,
            setfsuid,
            setgid,
            setgroups,
            sethostname,
            setitimer,
            set_mempolicy,
            setns,
            setpgid,
            setpriority,
            setregid,
            setresgid,
            setresuid,
            setreuid,
            setrlimit,
            set_robust_list,
            setsid,
            setsockopt,
            set_tid_address,
            settimeofday,
            setuid,
            setxattr,
            shmat,
            shmctl,
            shmdt,
            shmget,
            shutdown,
            sigaltstack,
            signalfd4,
            socket,
            socketpair,
            splice,
            statfs,
            statx,
            swapoff,
            swapon,
            symlinkat,
            sync,
            sync_file_range,
            syncfs,
            sysinfo,
            syslog,
            tee,
            tgkill,
            timer_create,
            timer_delete,
            timerfd_create,
            timerfd_gettime,
            timerfd_settime,
            timer_getoverrun,
            timer_gettime,
            timer_settime,
            times,
            tkill,
            truncate,
            umask,
            umount2,
            uname,
            unlinkat,
            unshare,
            userfaultfd,
            utimensat,
            vhangup,
            vmsplice,
            wait4,
            waitid,
            write,
            writev,
        }
    }
}

/// A system call declaration.
///
/// Describes the name of the syscall and its number.
#[derive(Copy, Clone)]
pub struct SyscallDecl {
    pub number: u64,
    pub name: &'static str,
}

impl std::fmt::Debug for SyscallDecl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.name, self.number)
    }
}

/// A particular invocation of a system call.
///
/// Contains the declaration of the invoked system call, as well as which arguments it was invoked
/// with.
pub struct Syscall {
    pub decl: SyscallDecl,
    pub arg0: SyscallArg,
    pub arg1: SyscallArg,
    pub arg2: SyscallArg,
    pub arg3: SyscallArg,
    pub arg4: SyscallArg,
    pub arg5: SyscallArg,
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

/// A macro for the body of SyscallDecl::from_number.
///
/// Evaluates to the &'static SyscallDecl for the given number or to
/// &DECL_UNKNOWN if the number is unknown.
macro_rules! syscall_match {
    {$number:ident; $($name:ident,)*} => {
        paste! {
            match $number as u32 {
                $([<__NR_ $name>] => stringify!($name),)*
                _ => "<unknown>",
            }
        }
    }
}

impl SyscallDecl {
    /// The SyscallDecl for the given syscall number.
    ///
    /// Returns &DECL_UNKNOWN if the given syscall number is not known.
    pub fn from_number(number: u64) -> SyscallDecl {
        let name = for_each_syscall! { syscall_match, number };
        Self { number, name }
    }
}

#[cfg(feature = "syscall_stats")]
mod syscall_stats {
    use crate::syscalls::decls::SyscallDecl;
    use starnix_uapi::*;

    use fuchsia_inspect as inspect;
    use once_cell::sync::Lazy;
    use paste::paste;

    /// A macro for declaring a SyscallDecl stats property.
    macro_rules! syscall_stats_property {
        ($($name:ident,)*) => {
            paste!{
                $(
                    static [<SYSCALL_ $name:upper _STATS>]: Lazy<inspect::UintProperty> =
                    Lazy::new(|| SYSCALL_STATS_NODE.create_uint(stringify!($name), 0));
                )*
            }
        }
    }

    static SYSCALL_STATS_NODE: Lazy<inspect::Node> =
        Lazy::new(|| inspect::component::inspector().root().create_child("syscall_stats"));
    static SYSCALL_UNKNOWN_STATS: Lazy<inspect::UintProperty> =
        Lazy::new(|| SYSCALL_STATS_NODE.create_uint("<unknown>", 0));

    // Produce each syscall stats property.
    for_each_syscall! {syscall_stats_property}

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
        pub fn stats_property(number: u64) -> &'static inspect::UintProperty {
            for_each_syscall! { syscall_match_stats, number }
        }
    }
}
