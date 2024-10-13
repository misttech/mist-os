// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_VFS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_VFS_H_

#include <lib/mistos/util/bitflags.h>
#include <zircon/types.h>

#include <asm/statfs.h>
#include <linux/eventpoll.h>
#include <linux/openat2.h>
#include <linux/poll.h>

namespace starnix_uapi {

struct statfs default_statfs(uint32_t magic);

enum class FdEventsEnum : uint32_t {
  PollIn = POLLIN,      // Data to read
  PollPri = POLLPRI,    // Urgent data to read
  PollOut = POLLOUT,    // Writing now will not block
  PollErr = POLLERR,    // Error condition
  PollHup = POLLHUP,    // Hung up
  PollNval = POLLNVAL,  // Invalid request

  // Additional constants from uapi
  PollRdNorm = POLLRDNORM,  // Normal data to read
  PollRdBand = POLLRDBAND,  // Priority band data to read
  PollWrNorm = POLLWRNORM,  // Writing will not block
  PollWrBand = POLLWRBAND,  // Priority data can be written
  PollMsg = POLLMSG,        // For STREAMS messages
  PollRemove = POLLREMOVE,  // Device removal notification
  PollRdHup = POLLRDHUP,    // Stream socket peer closed connection

  // epoll constants
  EpollEdgeTriggered = EPOLLET,  // Edge-triggered behavior
  EpollOneShot = EPOLLONESHOT,   // One-shot behavior
  EpollWakeUp = EPOLLWAKEUP      // Wake up the system if it is sleeping
};

using FdEvents = Flags<FdEventsEnum>;

class FdEventsImpl {
 public:
  static FdEvents from_u64(uint64_t value) {
    return FdEvents::from_bits_truncate(
        static_cast<uint32_t>(value & static_cast<uint64_t>(INT32_MAX)));
  }
};

enum class ResolveFlagsEnum : uint32_t {
  NO_XDEV = RESOLVE_NO_XDEV,
  NO_MAGICLINKS = RESOLVE_NO_MAGICLINKS,
  NO_SYMLINKS = RESOLVE_NO_SYMLINKS,
  BENEATH = RESOLVE_BENEATH,
  IN_ROOT = RESOLVE_IN_ROOT,
  CACHED = RESOLVE_CACHED,
};

using ResolveFlags = Flags<ResolveFlagsEnum>;

}  // namespace starnix_uapi

template <>
constexpr Flag<starnix_uapi::FdEventsEnum> Flags<starnix_uapi::FdEventsEnum>::FLAGS[] = {
    {starnix_uapi::FdEventsEnum::PollIn},    // POLLIN
    {starnix_uapi::FdEventsEnum::PollPri},   // POLLPRI
    {starnix_uapi::FdEventsEnum::PollOut},   // POLLOUT
    {starnix_uapi::FdEventsEnum::PollErr},   // POLLERR
    {starnix_uapi::FdEventsEnum::PollHup},   // POLLHUP
    {starnix_uapi::FdEventsEnum::PollNval},  // POLLNVAL

    // Additional poll events
    {starnix_uapi::FdEventsEnum::PollRdNorm},  // POLLRDNORM
    {starnix_uapi::FdEventsEnum::PollRdBand},  // POLLRDBAND
    {starnix_uapi::FdEventsEnum::PollWrNorm},  // POLLWRNORM
    {starnix_uapi::FdEventsEnum::PollWrBand},  // POLLWRBAND
    {starnix_uapi::FdEventsEnum::PollMsg},     // POLLMSG
    {starnix_uapi::FdEventsEnum::PollRemove},  // POLLREMOVE
    {starnix_uapi::FdEventsEnum::PollRdHup},   // POLLRDHUP

    // epoll events
    {starnix_uapi::FdEventsEnum::EpollEdgeTriggered},  // EPOLLET
    {starnix_uapi::FdEventsEnum::EpollOneShot},        // EPOLLONESHOT
    {starnix_uapi::FdEventsEnum::EpollWakeUp},         // EPOLLWAKEUP
};

template <>
constexpr Flag<starnix_uapi::ResolveFlagsEnum> Flags<starnix_uapi::ResolveFlagsEnum>::FLAGS[] = {
    {starnix_uapi::ResolveFlagsEnum::NO_XDEV},     {starnix_uapi::ResolveFlagsEnum::NO_MAGICLINKS},
    {starnix_uapi::ResolveFlagsEnum::NO_SYMLINKS}, {starnix_uapi::ResolveFlagsEnum::BENEATH},
    {starnix_uapi::ResolveFlagsEnum::IN_ROOT},     {starnix_uapi::ResolveFlagsEnum::CACHED},
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_VFS_H_
