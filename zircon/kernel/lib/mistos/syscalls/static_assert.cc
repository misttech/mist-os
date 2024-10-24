// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// sed -E 's/#define ([A-Za-z0-9_]+) ([0-9]+)/static_assert(\1 == \2);/'
// out/default/fidling/gen/zircon/kernel/lib/mistos/vdso/linux/zither/mistos/lib/syscalls/zx-syscall-numbers.h
// > zircon/kernel/lib/mistos/syscalls/static_assert.cc

#include <lib/syscalls/zx-syscall-numbers.h>

static_assert(ZX_SYS_read == 0);
static_assert(ZX_SYS_write == 1);
static_assert(ZX_SYS_open == 2);
static_assert(ZX_SYS_close == 3);
static_assert(ZX_SYS_stat == 4);
static_assert(ZX_SYS_fstat == 5);
static_assert(ZX_SYS_lstat == 6);
static_assert(ZX_SYS_poll == 7);
static_assert(ZX_SYS_lseek == 8);
static_assert(ZX_SYS_mmap == 9);
static_assert(ZX_SYS_mprotect == 10);
static_assert(ZX_SYS_munmap == 11);
static_assert(ZX_SYS_brk == 12);
static_assert(ZX_SYS_rt_sigaction == 13);
static_assert(ZX_SYS_rt_sigprocmask == 14);
static_assert(ZX_SYS_rt_sigreturn == 15);
static_assert(ZX_SYS_ioctl == 16);
static_assert(ZX_SYS_pread64 == 17);
static_assert(ZX_SYS_pwrite64 == 18);
static_assert(ZX_SYS_readv == 19);
static_assert(ZX_SYS_writev == 20);
static_assert(ZX_SYS_access == 21);
static_assert(ZX_SYS_pipe == 22);
static_assert(ZX_SYS_select == 23);
static_assert(ZX_SYS_sched_yield == 24);
static_assert(ZX_SYS_mremap == 25);
static_assert(ZX_SYS_msync == 26);
static_assert(ZX_SYS_mincore == 27);
static_assert(ZX_SYS_madvise == 28);
static_assert(ZX_SYS_shmget == 29);
static_assert(ZX_SYS_shmat == 30);
static_assert(ZX_SYS_shmctl == 31);
static_assert(ZX_SYS_dup == 32);
static_assert(ZX_SYS_dup2 == 33);
static_assert(ZX_SYS_pause == 34);
static_assert(ZX_SYS_nanosleep == 35);
static_assert(ZX_SYS_getitimer == 36);
static_assert(ZX_SYS_alarm == 37);
static_assert(ZX_SYS_setitimer == 38);
static_assert(ZX_SYS_getpid == 39);
static_assert(ZX_SYS_sendfile == 40);
static_assert(ZX_SYS_socket == 41);
static_assert(ZX_SYS_connect == 42);
static_assert(ZX_SYS_accept == 43);
static_assert(ZX_SYS_sendto == 44);
static_assert(ZX_SYS_recvfrom == 45);
static_assert(ZX_SYS_sendmsg == 46);
static_assert(ZX_SYS_recvmsg == 47);
static_assert(ZX_SYS_shutdown == 48);
static_assert(ZX_SYS_bind == 49);
static_assert(ZX_SYS_listen == 50);
static_assert(ZX_SYS_getsockname == 51);
static_assert(ZX_SYS_getpeername == 52);
static_assert(ZX_SYS_socketpair == 53);
static_assert(ZX_SYS_setsockopt == 54);
static_assert(ZX_SYS_getsockopt == 55);
static_assert(ZX_SYS_clone == 56);
static_assert(ZX_SYS_fork == 57);
static_assert(ZX_SYS_vfork == 58);
static_assert(ZX_SYS_execve == 59);
static_assert(ZX_SYS_exit == 60);
static_assert(ZX_SYS_wait4 == 61);
static_assert(ZX_SYS_kill == 62);
static_assert(ZX_SYS_uname == 63);
static_assert(ZX_SYS_semget == 64);
static_assert(ZX_SYS_semop == 65);
static_assert(ZX_SYS_semctl == 66);
static_assert(ZX_SYS_shmdt == 67);
static_assert(ZX_SYS_msgget == 68);
static_assert(ZX_SYS_msgsnd == 69);
static_assert(ZX_SYS_msgrcv == 70);
static_assert(ZX_SYS_msgctl == 71);
static_assert(ZX_SYS_fcntl == 72);
static_assert(ZX_SYS_flock == 73);
static_assert(ZX_SYS_fsync == 74);
static_assert(ZX_SYS_fdatasync == 75);
static_assert(ZX_SYS_truncate == 76);
static_assert(ZX_SYS_ftruncate == 77);
static_assert(ZX_SYS_getdents == 78);
static_assert(ZX_SYS_getcwd == 79);
static_assert(ZX_SYS_chdir == 80);
static_assert(ZX_SYS_fchdir == 81);
static_assert(ZX_SYS_rename == 82);
static_assert(ZX_SYS_mkdir == 83);
static_assert(ZX_SYS_rmdir == 84);
static_assert(ZX_SYS_creat == 85);
static_assert(ZX_SYS_link == 86);
static_assert(ZX_SYS_unlink == 87);
static_assert(ZX_SYS_symlink == 88);
static_assert(ZX_SYS_readlink == 89);
static_assert(ZX_SYS_chmod == 90);
static_assert(ZX_SYS_fchmod == 91);
static_assert(ZX_SYS_chown == 92);
static_assert(ZX_SYS_fchown == 93);
static_assert(ZX_SYS_lchown == 94);
static_assert(ZX_SYS_umask == 95);
static_assert(ZX_SYS_gettimeofday == 96);
static_assert(ZX_SYS_getrlimit == 97);
static_assert(ZX_SYS_getrusage == 98);
static_assert(ZX_SYS_sysinfo == 99);
static_assert(ZX_SYS_times == 100);
static_assert(ZX_SYS_ptrace == 101);
static_assert(ZX_SYS_getuid == 102);
static_assert(ZX_SYS_syslog == 103);
static_assert(ZX_SYS_getgid == 104);
static_assert(ZX_SYS_setuid == 105);
static_assert(ZX_SYS_setgid == 106);
static_assert(ZX_SYS_geteuid == 107);
static_assert(ZX_SYS_getegid == 108);
static_assert(ZX_SYS_setpgid == 109);
static_assert(ZX_SYS_getppid == 110);
static_assert(ZX_SYS_getpgrp == 111);
static_assert(ZX_SYS_setsid == 112);
static_assert(ZX_SYS_setreuid == 113);
static_assert(ZX_SYS_setregid == 114);
static_assert(ZX_SYS_getgroups == 115);
static_assert(ZX_SYS_setgroups == 116);
static_assert(ZX_SYS_setresuid == 117);
static_assert(ZX_SYS_getresuid == 118);
static_assert(ZX_SYS_setresgid == 119);
static_assert(ZX_SYS_getresgid == 120);
static_assert(ZX_SYS_getpgid == 121);
static_assert(ZX_SYS_setfsuid == 122);
static_assert(ZX_SYS_setfsgid == 123);
static_assert(ZX_SYS_getsid == 124);
static_assert(ZX_SYS_capget == 125);
static_assert(ZX_SYS_capset == 126);
static_assert(ZX_SYS_rt_sigpending == 127);
static_assert(ZX_SYS_rt_sigtimedwait == 128);
static_assert(ZX_SYS_rt_sigqueueinfo == 129);
static_assert(ZX_SYS_rt_sigsuspend == 130);
static_assert(ZX_SYS_sigaltstack == 131);
static_assert(ZX_SYS_utime == 132);
static_assert(ZX_SYS_mknod == 133);
static_assert(ZX_SYS_uselib == 134);
static_assert(ZX_SYS_personality == 135);
static_assert(ZX_SYS_ustat == 136);
static_assert(ZX_SYS_statfs == 137);
static_assert(ZX_SYS_fstatfs == 138);
static_assert(ZX_SYS_sysfs == 139);
static_assert(ZX_SYS_getpriority == 140);
static_assert(ZX_SYS_setpriority == 141);
static_assert(ZX_SYS_sched_setparam == 142);
static_assert(ZX_SYS_sched_getparam == 143);
static_assert(ZX_SYS_sched_setscheduler == 144);
static_assert(ZX_SYS_sched_getscheduler == 145);
static_assert(ZX_SYS_sched_get_priority_max == 146);
static_assert(ZX_SYS_sched_get_priority_min == 147);
static_assert(ZX_SYS_sched_rr_get_interval == 148);
static_assert(ZX_SYS_mlock == 149);
static_assert(ZX_SYS_munlock == 150);
static_assert(ZX_SYS_mlockall == 151);
static_assert(ZX_SYS_munlockall == 152);
static_assert(ZX_SYS_vhangup == 153);
static_assert(ZX_SYS_modify_ldt == 154);
static_assert(ZX_SYS_pivot_root == 155);
static_assert(ZX_SYS_ni_156 == 156);
static_assert(ZX_SYS_prctl == 157);
static_assert(ZX_SYS_arch_prctl == 158);
static_assert(ZX_SYS_adjtimex == 159);
static_assert(ZX_SYS_setrlimit == 160);
static_assert(ZX_SYS_chroot == 161);
static_assert(ZX_SYS_sync == 162);
static_assert(ZX_SYS_acct == 163);
static_assert(ZX_SYS_settimeofday == 164);
static_assert(ZX_SYS_mount == 165);
static_assert(ZX_SYS_umount2 == 166);
static_assert(ZX_SYS_swapon == 167);
static_assert(ZX_SYS_swapoff == 168);
static_assert(ZX_SYS_reboot == 169);
static_assert(ZX_SYS_sethostname == 170);
static_assert(ZX_SYS_setdomainname == 171);
static_assert(ZX_SYS_iopl == 172);
static_assert(ZX_SYS_ioperm == 173);
static_assert(ZX_SYS_create_module == 174);
static_assert(ZX_SYS_init_module == 175);
static_assert(ZX_SYS_delete_module == 176);
static_assert(ZX_SYS_get_kernel_syms == 177);
static_assert(ZX_SYS_query_module == 178);
static_assert(ZX_SYS_quotactl == 179);
static_assert(ZX_SYS_nfsservctl == 180);
static_assert(ZX_SYS_getpmsg == 181);
static_assert(ZX_SYS_putpmsg == 182);
static_assert(ZX_SYS_afs_syscall == 183);
static_assert(ZX_SYS_tuxcall == 184);
static_assert(ZX_SYS_security == 185);
static_assert(ZX_SYS_gettid == 186);
static_assert(ZX_SYS_readahead == 187);
static_assert(ZX_SYS_setxattr == 188);
static_assert(ZX_SYS_lsetxattr == 189);
static_assert(ZX_SYS_fsetxattr == 190);
static_assert(ZX_SYS_getxattr == 191);
static_assert(ZX_SYS_lgetxattr == 192);
static_assert(ZX_SYS_fgetxattr == 193);
static_assert(ZX_SYS_listxattr == 194);
static_assert(ZX_SYS_llistxattr == 195);
static_assert(ZX_SYS_flistxattr == 196);
static_assert(ZX_SYS_removexattr == 197);
static_assert(ZX_SYS_lremovexattr == 198);
static_assert(ZX_SYS_fremovexattr == 199);
static_assert(ZX_SYS_tkill == 200);
static_assert(ZX_SYS_time == 201);
static_assert(ZX_SYS_futex == 202);
static_assert(ZX_SYS_sched_setaffinity == 203);
static_assert(ZX_SYS_sched_getaffinity == 204);
static_assert(ZX_SYS_set_thread_area == 205);
static_assert(ZX_SYS_io_setup == 206);
static_assert(ZX_SYS_io_destroy == 207);
static_assert(ZX_SYS_io_getevents == 208);
static_assert(ZX_SYS_io_submit == 209);
static_assert(ZX_SYS_io_cancel == 210);
static_assert(ZX_SYS_get_thread_area == 211);
static_assert(ZX_SYS_lookup_dcookie == 212);
static_assert(ZX_SYS_epoll_create == 213);
static_assert(ZX_SYS_epoll_ctl_old == 214);
static_assert(ZX_SYS_epoll_wait_old == 215);
static_assert(ZX_SYS_remap_file_pages == 216);
static_assert(ZX_SYS_getdents64 == 217);
static_assert(ZX_SYS_set_tid_address == 218);
static_assert(ZX_SYS_restart_syscall == 219);
static_assert(ZX_SYS_semtimedop == 220);
static_assert(ZX_SYS_fadvise64 == 221);
static_assert(ZX_SYS_timer_create == 222);
static_assert(ZX_SYS_timer_settime == 223);
static_assert(ZX_SYS_timer_gettime == 224);
static_assert(ZX_SYS_timer_getoverrun == 225);
static_assert(ZX_SYS_timer_delete == 226);
static_assert(ZX_SYS_clock_settime == 227);
static_assert(ZX_SYS_clock_gettime == 228);
static_assert(ZX_SYS_clock_getres == 229);
static_assert(ZX_SYS_clock_nanosleep == 230);
static_assert(ZX_SYS_exit_group == 231);
static_assert(ZX_SYS_epoll_wait == 232);
static_assert(ZX_SYS_epoll_ctl == 233);
static_assert(ZX_SYS_tgkill == 234);
static_assert(ZX_SYS_utimes == 235);
static_assert(ZX_SYS_vserver == 236);
static_assert(ZX_SYS_mbind == 237);
static_assert(ZX_SYS_set_mempolicy == 238);
static_assert(ZX_SYS_get_mempolicy == 239);
static_assert(ZX_SYS_mq_open == 240);
static_assert(ZX_SYS_mq_unlink == 241);
static_assert(ZX_SYS_mq_timedsend == 242);
static_assert(ZX_SYS_mq_timedreceive == 243);
static_assert(ZX_SYS_mq_notify == 244);
static_assert(ZX_SYS_mq_getsetattr == 245);
static_assert(ZX_SYS_kexec_load == 246);
static_assert(ZX_SYS_waitid == 247);
static_assert(ZX_SYS_add_key == 248);
static_assert(ZX_SYS_request_key == 249);
static_assert(ZX_SYS_keyctl == 250);
static_assert(ZX_SYS_ioprio_set == 251);
static_assert(ZX_SYS_ioprio_get == 252);
static_assert(ZX_SYS_inotify_init == 253);
static_assert(ZX_SYS_inotify_add_watch == 254);
static_assert(ZX_SYS_inotify_rm_watch == 255);
static_assert(ZX_SYS_migrate_pages == 256);
static_assert(ZX_SYS_openat == 257);
static_assert(ZX_SYS_mkdirat == 258);
static_assert(ZX_SYS_mknodat == 259);
static_assert(ZX_SYS_fchownat == 260);
static_assert(ZX_SYS_futimesat == 261);
static_assert(ZX_SYS_newfstatat == 262);
static_assert(ZX_SYS_unlinkat == 263);
static_assert(ZX_SYS_renameat == 264);
static_assert(ZX_SYS_linkat == 265);
static_assert(ZX_SYS_symlinkat == 266);
static_assert(ZX_SYS_readlinkat == 267);
static_assert(ZX_SYS_fchmodat == 268);
static_assert(ZX_SYS_faccessat == 269);
static_assert(ZX_SYS_pselect6 == 270);
static_assert(ZX_SYS_ppoll == 271);
static_assert(ZX_SYS_unshare == 272);
static_assert(ZX_SYS_set_robust_list == 273);
static_assert(ZX_SYS_get_robust_list == 274);
static_assert(ZX_SYS_splice == 275);
static_assert(ZX_SYS_tee == 276);
static_assert(ZX_SYS_sync_file_range == 277);
static_assert(ZX_SYS_vmsplice == 278);
static_assert(ZX_SYS_move_pages == 279);
static_assert(ZX_SYS_utimensat == 280);
static_assert(ZX_SYS_epoll_pwait == 281);
static_assert(ZX_SYS_signalfd == 282);
static_assert(ZX_SYS_timerfd_create == 283);
static_assert(ZX_SYS_eventfd == 284);
static_assert(ZX_SYS_fallocate == 285);
static_assert(ZX_SYS_timerfd_settime == 286);
static_assert(ZX_SYS_timerfd_gettime == 287);
static_assert(ZX_SYS_accept4 == 288);
static_assert(ZX_SYS_signalfd4 == 289);
static_assert(ZX_SYS_eventfd2 == 290);
static_assert(ZX_SYS_epoll_create1 == 291);
static_assert(ZX_SYS_dup3 == 292);
static_assert(ZX_SYS_pipe2 == 293);
static_assert(ZX_SYS_inotify_init1 == 294);
static_assert(ZX_SYS_preadv == 295);
static_assert(ZX_SYS_pwritev == 296);
static_assert(ZX_SYS_rt_tgsigqueueinfo == 297);
static_assert(ZX_SYS_perf_event_open == 298);
static_assert(ZX_SYS_recvmmsg == 299);
static_assert(ZX_SYS_fanotify_init == 300);
static_assert(ZX_SYS_fanotify_mark == 301);
static_assert(ZX_SYS_prlimit64 == 302);
static_assert(ZX_SYS_name_to_handle_at == 303);
static_assert(ZX_SYS_open_by_handle_at == 304);
static_assert(ZX_SYS_clock_adjtime == 305);
static_assert(ZX_SYS_syncfs == 306);
static_assert(ZX_SYS_sendmmsg == 307);
static_assert(ZX_SYS_setns == 308);
static_assert(ZX_SYS_getcpu == 309);
static_assert(ZX_SYS_process_vm_readv == 310);
static_assert(ZX_SYS_process_vm_writev == 311);
static_assert(ZX_SYS_kcmp == 312);
static_assert(ZX_SYS_finit_module == 313);
static_assert(ZX_SYS_sched_setattr == 314);
static_assert(ZX_SYS_sched_getattr == 315);
static_assert(ZX_SYS_renameat2 == 316);
static_assert(ZX_SYS_seccomp == 317);
static_assert(ZX_SYS_getrandom == 318);
static_assert(ZX_SYS_memfd_create == 319);
static_assert(ZX_SYS_kexec_file_load == 320);
static_assert(ZX_SYS_bpf == 321);
static_assert(ZX_SYS_execveat == 322);
static_assert(ZX_SYS_userfaultfd == 323);
static_assert(ZX_SYS_membarrier == 324);
static_assert(ZX_SYS_mlock2 == 325);
static_assert(ZX_SYS_copy_file_range == 326);
static_assert(ZX_SYS_preadv2 == 327);
static_assert(ZX_SYS_pwritev2 == 328);
static_assert(ZX_SYS_pkey_mprotect == 329);
static_assert(ZX_SYS_pkey_alloc == 330);
static_assert(ZX_SYS_pkey_free == 331);
static_assert(ZX_SYS_statx == 332);
static_assert(ZX_SYS_io_pgetevents == 333);
static_assert(ZX_SYS_rseq == 334);
static_assert(ZX_SYS_blank_335 == 335);
static_assert(ZX_SYS_blank_336 == 336);
static_assert(ZX_SYS_blank_337 == 337);
static_assert(ZX_SYS_blank_338 == 338);
static_assert(ZX_SYS_blank_339 == 339);
static_assert(ZX_SYS_blank_340 == 340);
static_assert(ZX_SYS_blank_341 == 341);
static_assert(ZX_SYS_blank_342 == 342);
static_assert(ZX_SYS_blank_343 == 343);
static_assert(ZX_SYS_blank_344 == 344);
static_assert(ZX_SYS_blank_345 == 345);
static_assert(ZX_SYS_blank_346 == 346);
static_assert(ZX_SYS_blank_347 == 347);
static_assert(ZX_SYS_blank_348 == 348);
static_assert(ZX_SYS_blank_349 == 349);
static_assert(ZX_SYS_blank_350 == 350);
static_assert(ZX_SYS_blank_351 == 351);
static_assert(ZX_SYS_blank_352 == 352);
static_assert(ZX_SYS_blank_353 == 353);
static_assert(ZX_SYS_blank_354 == 354);
static_assert(ZX_SYS_blank_355 == 355);
static_assert(ZX_SYS_blank_356 == 356);
static_assert(ZX_SYS_blank_357 == 357);
static_assert(ZX_SYS_blank_358 == 358);
static_assert(ZX_SYS_blank_359 == 359);
static_assert(ZX_SYS_blank_360 == 360);
static_assert(ZX_SYS_blank_361 == 361);
static_assert(ZX_SYS_blank_362 == 362);
static_assert(ZX_SYS_blank_363 == 363);
static_assert(ZX_SYS_blank_364 == 364);
static_assert(ZX_SYS_blank_365 == 365);
static_assert(ZX_SYS_blank_366 == 366);
static_assert(ZX_SYS_blank_367 == 367);
static_assert(ZX_SYS_blank_368 == 368);
static_assert(ZX_SYS_blank_369 == 369);
static_assert(ZX_SYS_blank_370 == 370);
static_assert(ZX_SYS_blank_371 == 371);
static_assert(ZX_SYS_blank_372 == 372);
static_assert(ZX_SYS_blank_373 == 373);
static_assert(ZX_SYS_blank_374 == 374);
static_assert(ZX_SYS_blank_375 == 375);
static_assert(ZX_SYS_blank_376 == 376);
static_assert(ZX_SYS_blank_377 == 377);
static_assert(ZX_SYS_blank_378 == 378);
static_assert(ZX_SYS_blank_379 == 379);
static_assert(ZX_SYS_blank_380 == 380);
static_assert(ZX_SYS_blank_381 == 381);
static_assert(ZX_SYS_blank_382 == 382);
static_assert(ZX_SYS_blank_383 == 383);
static_assert(ZX_SYS_blank_384 == 384);
static_assert(ZX_SYS_blank_385 == 385);
static_assert(ZX_SYS_blank_386 == 386);
static_assert(ZX_SYS_dont_use_387 == 387);
static_assert(ZX_SYS_dont_use_388 == 388);
static_assert(ZX_SYS_dont_use_389 == 389);
static_assert(ZX_SYS_dont_use_390 == 390);
static_assert(ZX_SYS_dont_use_391 == 391);
static_assert(ZX_SYS_dont_use_392 == 392);
static_assert(ZX_SYS_dont_use_393 == 393);
static_assert(ZX_SYS_dont_use_394 == 394);
static_assert(ZX_SYS_dont_use_395 == 395);
static_assert(ZX_SYS_dont_use_396 == 396);
static_assert(ZX_SYS_dont_use_397 == 397);
static_assert(ZX_SYS_dont_use_398 == 398);
static_assert(ZX_SYS_dont_use_399 == 399);
static_assert(ZX_SYS_dont_use_400 == 400);
static_assert(ZX_SYS_dont_use_401 == 401);
static_assert(ZX_SYS_dont_use_402 == 402);
static_assert(ZX_SYS_dont_use_403 == 403);
static_assert(ZX_SYS_dont_use_404 == 404);
static_assert(ZX_SYS_dont_use_405 == 405);
static_assert(ZX_SYS_dont_use_406 == 406);
static_assert(ZX_SYS_dont_use_407 == 407);
static_assert(ZX_SYS_dont_use_408 == 408);
static_assert(ZX_SYS_dont_use_409 == 409);
static_assert(ZX_SYS_dont_use_410 == 410);
static_assert(ZX_SYS_dont_use_411 == 411);
static_assert(ZX_SYS_dont_use_412 == 412);
static_assert(ZX_SYS_dont_use_413 == 413);
static_assert(ZX_SYS_dont_use_414 == 414);
static_assert(ZX_SYS_dont_use_415 == 415);
static_assert(ZX_SYS_dont_use_416 == 416);
static_assert(ZX_SYS_dont_use_417 == 417);
static_assert(ZX_SYS_dont_use_418 == 418);
static_assert(ZX_SYS_dont_use_419 == 419);
static_assert(ZX_SYS_dont_use_420 == 420);
static_assert(ZX_SYS_dont_use_421 == 421);
static_assert(ZX_SYS_dont_use_422 == 422);
static_assert(ZX_SYS_dont_use_423 == 423);
static_assert(ZX_SYS_pidfd_send_signal == 424);
static_assert(ZX_SYS_io_uring_setup == 425);
static_assert(ZX_SYS_io_uring_enter == 426);
static_assert(ZX_SYS_io_uring_register == 427);
static_assert(ZX_SYS_open_tree == 428);
static_assert(ZX_SYS_move_mount == 429);
static_assert(ZX_SYS_fsopen == 430);
static_assert(ZX_SYS_fsconfig == 431);
static_assert(ZX_SYS_fsmount == 432);
static_assert(ZX_SYS_fspick == 433);
static_assert(ZX_SYS_pidfd_open == 434);
static_assert(ZX_SYS_clone3 == 435);
static_assert(ZX_SYS_close_range == 436);
static_assert(ZX_SYS_openat2 == 437);
static_assert(ZX_SYS_pidfd_getfd == 438);
static_assert(ZX_SYS_faccessat2 == 439);
static_assert(ZX_SYS_process_madvise == 440);
static_assert(ZX_SYS_epoll_pwait2 == 441);
static_assert(ZX_SYS_mount_setattr == 442);
static_assert(ZX_SYS_quotactl_fd == 443);
static_assert(ZX_SYS_landlock_create_ruleset == 444);
static_assert(ZX_SYS_landlock_add_rule == 445);
static_assert(ZX_SYS_landlock_restrict_self == 446);
static_assert(ZX_SYS_memfd_secret == 447);
static_assert(ZX_SYS_process_mrelease == 448);
static_assert(ZX_SYS_futex_waitv == 449);
static_assert(ZX_SYS_set_mempolicy_home_node == 450);
static_assert(ZX_SYS_cachestat == 451);
static_assert(ZX_SYS_fchmodat2 == 452);
static_assert(ZX_SYS_map_shadow_stack == 453);
static_assert(ZX_SYS_futex_wake == 454);
static_assert(ZX_SYS_futex_wait == 455);
static_assert(ZX_SYS_futex_requeue == 456);
static_assert(ZX_SYS_statmount == 457);
static_assert(ZX_SYS_listmount == 458);
static_assert(ZX_SYS_lsm_get_self_attr == 459);
static_assert(ZX_SYS_lsm_set_self_attr == 460);
static_assert(ZX_SYS_lsm_list_modules == 461);
static_assert(ZX_SYS_blank_462 == 462);
static_assert(ZX_SYS_blank_463 == 463);
static_assert(ZX_SYS_blank_464 == 464);
static_assert(ZX_SYS_blank_465 == 465);
static_assert(ZX_SYS_blank_466 == 466);
static_assert(ZX_SYS_blank_467 == 467);
static_assert(ZX_SYS_blank_468 == 468);
static_assert(ZX_SYS_blank_469 == 469);
static_assert(ZX_SYS_blank_470 == 470);
static_assert(ZX_SYS_blank_471 == 471);
static_assert(ZX_SYS_blank_472 == 472);
static_assert(ZX_SYS_blank_473 == 473);
static_assert(ZX_SYS_blank_474 == 474);
static_assert(ZX_SYS_blank_475 == 475);
static_assert(ZX_SYS_blank_476 == 476);
static_assert(ZX_SYS_blank_477 == 477);
static_assert(ZX_SYS_blank_478 == 478);
static_assert(ZX_SYS_blank_479 == 479);
static_assert(ZX_SYS_blank_480 == 480);
static_assert(ZX_SYS_blank_481 == 481);
static_assert(ZX_SYS_blank_482 == 482);
static_assert(ZX_SYS_blank_483 == 483);
static_assert(ZX_SYS_blank_484 == 484);
static_assert(ZX_SYS_blank_485 == 485);
static_assert(ZX_SYS_blank_486 == 486);
static_assert(ZX_SYS_blank_487 == 487);
static_assert(ZX_SYS_blank_488 == 488);
static_assert(ZX_SYS_blank_489 == 489);
static_assert(ZX_SYS_blank_490 == 490);
static_assert(ZX_SYS_blank_491 == 491);
static_assert(ZX_SYS_blank_492 == 492);
static_assert(ZX_SYS_blank_493 == 493);
static_assert(ZX_SYS_blank_494 == 494);
static_assert(ZX_SYS_blank_495 == 495);
static_assert(ZX_SYS_blank_496 == 496);
static_assert(ZX_SYS_blank_497 == 497);
static_assert(ZX_SYS_blank_498 == 498);
static_assert(ZX_SYS_blank_499 == 499);
static_assert(ZX_SYS_blank_500 == 500);
static_assert(ZX_SYS_blank_501 == 501);
static_assert(ZX_SYS_blank_502 == 502);
static_assert(ZX_SYS_blank_503 == 503);
static_assert(ZX_SYS_blank_504 == 504);
static_assert(ZX_SYS_blank_505 == 505);
static_assert(ZX_SYS_blank_506 == 506);
static_assert(ZX_SYS_blank_507 == 507);
static_assert(ZX_SYS_blank_508 == 508);
static_assert(ZX_SYS_blank_509 == 509);
static_assert(ZX_SYS_blank_510 == 510);
static_assert(ZX_SYS_blank_511 == 511);
static_assert(ZX_SYS_compat_rt_sigaction == 512);
static_assert(ZX_SYS_compat_rt_sigreturn == 513);
static_assert(ZX_SYS_compat_ioctl == 514);
static_assert(ZX_SYS_compat_readv == 515);
static_assert(ZX_SYS_compat_writev == 516);
static_assert(ZX_SYS_compat_recvfrom == 517);
static_assert(ZX_SYS_compat_sendmsg == 518);
static_assert(ZX_SYS_compat_recvmsg == 519);
static_assert(ZX_SYS_compat_execve == 520);
static_assert(ZX_SYS_compat_ptrace == 521);
static_assert(ZX_SYS_compat_rt_sigpending == 522);
static_assert(ZX_SYS_compat_rt_sigtimedwait == 523);
static_assert(ZX_SYS_compat_rt_sigqueueinfo == 524);
static_assert(ZX_SYS_compat_sigaltstack == 525);
static_assert(ZX_SYS_compat_timer_create == 526);
static_assert(ZX_SYS_compat_mq_notify == 527);
static_assert(ZX_SYS_compat_kexec_load == 528);
static_assert(ZX_SYS_compat_waitid == 529);
static_assert(ZX_SYS_compat_set_robust_list == 530);
static_assert(ZX_SYS_compat_get_robust_list == 531);
static_assert(ZX_SYS_compat_vmsplice == 532);
static_assert(ZX_SYS_compat_move_pages == 533);
static_assert(ZX_SYS_compat_preadv == 534);
static_assert(ZX_SYS_compat_pwritev == 535);
static_assert(ZX_SYS_compat_rt_tgsigqueueinfo == 536);
static_assert(ZX_SYS_compat_recvmmsg == 537);
static_assert(ZX_SYS_compat_sendmmsg == 538);
static_assert(ZX_SYS_compat_process_vm_readv == 539);
static_assert(ZX_SYS_compat_process_vm_writev == 540);
static_assert(ZX_SYS_compat_setsockopt == 541);
static_assert(ZX_SYS_compat_getsockopt == 542);
static_assert(ZX_SYS_compat_io_setup == 543);
static_assert(ZX_SYS_compat_io_submit == 544);
static_assert(ZX_SYS_compat_execveat == 545);
static_assert(ZX_SYS_compat_preadv2 == 546);
static_assert(ZX_SYS_compat_pwritev2 == 547);
/*static_assert(ZX_SYS_test_0 == 548);
static_assert(ZX_SYS_test_1 == 549);
static_assert(ZX_SYS_test_2 == 550);
static_assert(ZX_SYS_test_3 == 551);
static_assert(ZX_SYS_test_4 == 552);
static_assert(ZX_SYS_test_5 == 553);
static_assert(ZX_SYS_test_6 == 554);
static_assert(ZX_SYS_COUNT == 555);*/
