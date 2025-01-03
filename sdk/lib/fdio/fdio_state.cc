// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/fdio/fdio_state.h"

#include <fbl/auto_lock.h>

namespace {

fdio_slot* slot_locked(std::array<fdio_slot, FDIO_MAX_FD>& fdtab, int fd) {
  if ((fd < 0) || (fd >= FDIO_MAX_FD)) {
    return nullptr;
  }
  return &fdtab[fd];
}

}  // namespace

// `constinit` to force initialization at program load time. Otherwise initialization may occur
// after |__libc_extension_init|, wiping the fd table *after* it has been filled in with valid
// entries; musl invokes |__libc_start_init| after |__libc_extensions_init|.
//
// Note that even moving the initialization to |__libc_extensions_init| doesn't work out in the
// presence of sanitizers that deliberately initialize with garbage *after* |__libc_extensions_init|
// runs.
constinit fdio_state_t __fdio_global_state{
    .cwd_path = fdio_internal::PathBuffer('/'),
};

std::optional<int> fdio_state_t::bind_to_fd(const fbl::RefPtr<fdio>& io) {
  fbl::AutoLock guard(&lock);
  return bind_to_fd_locked(io);
}

std::optional<int> fdio_state_t::bind_to_fd_locked(const fbl::RefPtr<fdio>& io) {
  for (size_t fd = 0; fd < fdtab.size(); ++fd) {
    if (fdtab[fd].try_set(io)) {
      return fd;
    }
  }
  return std::nullopt;
}

fbl::RefPtr<fdio> fdio_state_t::fd_to_io(int fd) {
  fbl::AutoLock guard(&lock);
  return fd_to_io_locked(fd);
}

fbl::RefPtr<fdio> fdio_state_t::fd_to_io_locked(int fd) {
  fdio_slot* slot = slot_locked(fdtab, fd);
  if (slot == nullptr) {
    return nullptr;
  }
  return slot->get();
}

fbl::RefPtr<fdio> fdio_state_t::unbind_from_fd(int fd) {
  fbl::AutoLock guard(&lock);
  return unbind_from_fd_locked(fd);
}

fbl::RefPtr<fdio> fdio_state_t::unbind_from_fd_locked(int fd) {
  fdio_slot* slot = slot_locked(fdtab, fd);
  if (slot == nullptr) {
    return nullptr;
  }
  return slot->release();
}
