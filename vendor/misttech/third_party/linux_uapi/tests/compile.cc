// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/linux_uapi/typedefs.h>
#include <stddef.h>
#include <sys/socket.h>

#include <asm/ioctls.h>
#include <asm/poll.h>
#include <asm/sigcontext.h>
#include <asm/signal.h>
#include <asm/socket.h>
#include <asm/stat.h>
#include <asm/statfs.h>
#include <asm/ucontext.h>
#include <linux/aio_abi.h>
#include <linux/audit.h>
#include <linux/auxvec.h>
#include <linux/bpf.h>
#include <linux/bpf_perf_event.h>
#include <linux/capability.h>
#include <linux/close_range.h>
#include <linux/dm-ioctl.h>
#include <linux/errno.h>
#include <linux/eventpoll.h>
#include <linux/fadvise.h>
#include <linux/falloc.h>
#include <linux/fb.h>
#include <linux/fcntl.h>
#include <linux/fib_rules.h>
#include <linux/filter.h>
#include <linux/fs.h>
#include <linux/fsverity.h>
#include <linux/fuse.h>
#include <linux/futex.h>
#include <linux/if_arp.h>
#include <linux/if_packet.h>
#include <linux/inotify.h>
#include <linux/input.h>
#include <linux/ip.h>
#include <linux/ipv6.h>
#include <linux/kcmp.h>
#include <linux/limits.h>
#include <linux/loop.h>
#include <linux/magic.h>
#include <linux/membarrier.h>
#include <linux/memfd.h>
#include <linux/mman.h>
#include <linux/mqueue.h>
#include <linux/netfilter_ipv4/ip_tables.h>
#include <linux/netfilter_ipv6/ip6_tables.h>
#include <linux/netlink.h>
#include <linux/oom.h>
#include <linux/perf_event.h>
#include <linux/personality.h>
#include <linux/pfkeyv2.h>
#include <linux/pidfd.h>
#include <linux/poll.h>
#include <linux/prctl.h>
#include <linux/ptrace.h>
#include <linux/random.h>
#include <linux/reboot.h>
#include <linux/resource.h>
#include <linux/rtnetlink.h>
#include <linux/sched.h>
#include <linux/sched/types.h>
#include <linux/seccomp.h>
#include <linux/securebits.h>
#include <linux/signal.h>
#include <linux/signalfd.h>
#include <linux/socket.h>
#include <linux/sockios.h>
#include <linux/stat.h>
#include <linux/sync_file.h>
#include <linux/sysinfo.h>
#include <linux/termios.h>
#include <linux/time.h>
#include <linux/timerfd.h>
#include <linux/times.h>
#include <linux/uinput.h>
#include <linux/uio.h>
#include <linux/un.h>
#include <linux/unistd.h>
#include <linux/usb/functionfs.h>
#include <linux/utsname.h>
#include <linux/vm_sockets.h>
#include <linux/wait.h>
#include <linux/xattr.h>

#ifdef __x86_64__
#include <asm/prctl.h>
#endif

#include <fcntl.h>
