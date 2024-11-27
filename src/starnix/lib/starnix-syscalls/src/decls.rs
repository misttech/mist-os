// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use crate::SyscallArg;

/// Helper for for_each_syscall! that adds any architecture-specific syscalls.
///
/// X86_64 has many unique syscalls for legacy reasons. Newer architectures funnel some of these
/// through some newer and more general variants. The variants used by other platforms are listed in
/// the comments below.
#[cfg(target_arch = "x86_64")]
#[macro_export]
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
#[macro_export]
macro_rules! for_each_arch_syscall {
    {$callback:ident; $($context:ident;)* ; $($common_name:ident,)*} => {
        $callback!{
            $($context;)*
            $($common_name,)*
            renameat,  // renameat2
            syscalls,
        }
    }
}

#[cfg(target_arch = "riscv64")]
#[macro_export]
macro_rules! for_each_arch_syscall {
    {$callback:ident; $($context:ident;)* ; $($common_name:ident,)*} => {
        $callback!{
            $($context;)*
            $($common_name,)*
            syscalls,
        }
    }
}

/// Intended to be used with other macros to produce code that needs to handle
/// each syscall.
///
/// This list contains all cross-architecture syscalls, and delegates through for_each_arch_syscall!
/// to add in any architecture-specific ones.
#[macro_export]
macro_rules! for_each_syscall {
    {$callback:ident $(,$context:ident)*} => {
        $crate::for_each_arch_syscall!{
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
            cachestat,
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
            close_range,
            close,
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
            exit_group,
            exit,
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
            futex_waitv,
            futex,
            get_mempolicy,
            get_robust_list,
            getcpu,
            getcwd,
            getdents64,
            getegid,
            geteuid,
            getgid,
            getgroups,
            getitimer,
            getpeername,
            getpgid,
            getpid,
            getppid,
            getpriority,
            getrandom,
            getresgid,
            getresuid,
            getrlimit,
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
            io_destroy,
            io_getevents,
            io_pgetevents,
            io_setup,
            io_submit,
            io_uring_enter,
            io_uring_register,
            io_uring_setup,
            ioctl,
            ioprio_get,
            ioprio_set,
            kcmp,
            kexec_file_load,
            kexec_load,
            keyctl,
            kill,
            landlock_add_rule,
            landlock_create_ruleset,
            landlock_restrict_self,
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
            memfd_secret,
            migrate_pages,
            mincore,
            mkdirat,
            mknodat,
            mlock,
            mlock2,
            mlockall,
            mmap,
            mount_setattr,
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
            open_by_handle_at,
            open_tree,
            openat,
            openat2,
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
            process_mrelease,
            process_vm_readv,
            process_vm_writev,
            pselect6,
            ptrace,
            pwrite64,
            pwritev,
            pwritev2,
            quotactl_fd,
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
            sched_get_priority_max,
            sched_get_priority_min,
            sched_getaffinity,
            sched_getattr,
            sched_getparam,
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
            set_mempolicy_home_node,
            set_mempolicy,
            set_robust_list,
            set_tid_address,
            setdomainname,
            setfsgid,
            setfsuid,
            setgid,
            setgroups,
            sethostname,
            setitimer,
            setns,
            setpgid,
            setpriority,
            setregid,
            setresgid,
            setresuid,
            setreuid,
            setrlimit,
            setsid,
            setsockopt,
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
            sync_file_range,
            sync,
            syncfs,
            sysinfo,
            syslog,
            tee,
            tgkill,
            timer_create,
            timer_delete,
            timer_getoverrun,
            timer_gettime,
            timer_settime,
            timerfd_create,
            timerfd_gettime,
            timerfd_settime,
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

/// Evaluates to a string literal for the given syscall number when called back by for_each_syscall.
#[macro_export]
macro_rules! syscall_number_to_name_literal_callback {
    {$number:ident; $($name:ident,)*} => {
        $crate::__paste::paste! {
            match $number as u32 {
                $(starnix_uapi::[<__NR_ $name>] => stringify!($name),)*
                _ => "<unknown syscall>",
            }
        }
    }
}

impl SyscallDecl {
    /// The SyscallDecl for the given syscall number.
    ///
    /// Returns &DECL_UNKNOWN if the given syscall number is not known.
    pub fn from_number(number: u64) -> SyscallDecl {
        let name = for_each_syscall! { syscall_number_to_name_literal_callback, number };
        Self { number, name }
    }
}
