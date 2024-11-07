// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_LIB_LINUX_UAPI_STUB_ARM_ARCH32_SYSCALLS_H_
#define SRC_STARNIX_LIB_LINUX_UAPI_STUB_ARM_ARCH32_SYSCALLS_H_

// THe uapi for arm linux doesn't provide __NR_syscalls.
#define __NR_syscalls 452  // cachestate + 1
// TODO(drewry) Do we need to support the ARM specific calls:
#define __NR_ARM_breakpoint 983041
#define __NR_ARM_cacheflush 983042
#define __NR_ARM_set_tls 983045
#define __NR_ARM_usr26 983043
#define __NR_ARM_usr32 983044

#define __NR_arch32_open 5
#define __NR_arch32_execve 11
#define __NR_arch32_lseek 19
#define __NR_arch32_ptrace 26
#define __NR_arch32_times 43
#define __NR_arch32_ioctl 54
#define __NR_arch32_fcntl 55
#define __NR_arch32_ustat 62
#define __NR_arch32_sigaction 67
#define __NR_arch32_sigpending 73
#define __NR_arch32_setrlimit 75
#define __NR_arch32_getrusage 77
#define __NR_arch32_gettimeofday 78
#define __NR_arch32_settimeofday 79
#define __NR_arch32_truncate 92
#define __NR_arch32_ftruncate 93
#define __NR_arch32_statfs 99
#define __NR_arch32_fstatfs 100
#define __NR_arch32_setitimer 104
#define __NR_arch32_getitimer 105
#define __NR_arch32_newstat 106
#define __NR_arch32_newlstat 107
#define __NR_arch32_newfstat 108
#define __NR_arch32_wait4 114
#define __NR_arch32_sysinfo 116
#define __NR_arch32_sigreturn 119
#define __NR_arch32_sigprocmask 126
#define __NR_arch32_getdents 141
#define __NR_arch32_select 142
#define __NR_arch32_rt_sigreturn 173
#define __NR_arch32_rt_sigaction 174
#define __NR_arch32_rt_sigprocmask 175
#define __NR_arch32_rt_sigpending 176
#define __NR_arch32_rt_sigtimedwait_time32 177
#define __NR_arch32_rt_sigqueueinfo 178
#define __NR_arch32_rt_sigsuspend 179
#define __NR_arch32_aarch32_pread64 180
#define __NR_arch32_aarch32_pwrite64 181
#define __NR_arch32_sigaltstack 186
#define __NR_arch32_sendfile 187
#define __NR_arch32_getrlimit 191
#define __NR_arch32_aarch32_mmap2 192
#define __NR_arch32_aarch32_truncate64 193
#define __NR_arch32_aarch32_ftruncate64 194
#define __NR_arch32_fcntl64 221
#define __NR_arch32_aarch32_readahead 225
#define __NR_arch32_sched_setaffinity 241
#define __NR_arch32_sched_getaffinity 242
#define __NR_arch32_io_setup 243
#define __NR_arch32_io_submit 246
#define __NR_arch32_timer_create 257
#define __NR_arch32_aarch32_statfs64 266
#define __NR_arch32_aarch32_fstatfs64 267
#define __NR_arch32_aarch32_fadvise64_64 270
#define __NR_arch32_mq_open 274
#define __NR_arch32_mq_notify 278
#define __NR_arch32_mq_getsetattr 279
#define __NR_arch32_waitid 280
#define __NR_arch32_recv 291
#define __NR_arch32_recvfrom 292
#define __NR_arch32_sendmsg 296
#define __NR_arch32_recvmsg 297
#define __NR_arch32_old_semctl 300
#define __NR_arch32_msgsnd 301
#define __NR_arch32_msgrcv 302
#define __NR_arch32_old_msgctl 304
#define __NR_arch32_shmat 305
#define __NR_arch32_old_shmctl 308
#define __NR_arch32_keyctl 311
#define __NR_arch32_openat 322
#define __NR_arch32_pselect6_time32 335
#define __NR_arch32_ppoll_time32 336
#define __NR_arch32_set_robust_list 338
#define __NR_arch32_get_robust_list 339
#define __NR_arch32_aarch32_sync_file_range2 341
#define __NR_arch32_epoll_pwait 346
#define __NR_arch32_kexec_load 347
#define __NR_arch32_signalfd 349
#define __NR_arch32_aarch32_fallocate 352
#define __NR_arch32_signalfd4 355
#define __NR_arch32_preadv 361
#define __NR_arch32_pwritev 362
#define __NR_arch32_rt_tgsigqueueinfo 363
#define __NR_arch32_recvmmsg_time32 365
#define __NR_arch32_fanotify_mark 368
#define __NR_arch32_open_by_handle_at 371
#define __NR_arch32_sendmmsg 374
#define __NR_arch32_execveat 387
#define __NR_arch32_preadv2 392
#define __NR_arch32_pwritev2 393
#define __NR_arch32_io_pgetevents 399
#define __NR_arch32_pselect6_time64 413
#define __NR_arch32_ppoll_time64 414
#define __NR_arch32_io_pgetevents_time64 416
#define __NR_arch32_recvmmsg_time64 417
#define __NR_arch32_rt_sigtimedwait_time64 421
#define __NR_arch32_epoll_pwait2 441

#endif  // SRC_STARNIX_LIB_LINUX_UAPI_STUB_ARM_ARCH32_SYSCALLS_H_
