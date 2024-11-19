// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_CONTAINER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_CONTAINER_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/runner/config.h>
#include <lib/mistos/starnix_uapi/errors.h>

#include <fbl/ref_ptr.h>

namespace starnix {
class Kernel;
class FsContext;
}  // namespace starnix

using starnix_uapi::Errno;
namespace starnix_kernel_runner {

struct Container {
  /// The `Kernel` object that is associated with the container.
  fbl::RefPtr<starnix::Kernel> kernel;

  /// Inspect node holding information about the state of the container.
  //_node: inspect::Node,

  /// Until negative trait bound are implemented, using `*mut u8` to prevent transferring
  /// Container across threads.
  //_thread_bound: std::marker::PhantomData<*mut u8>,

  // C++
  ~Container();
};

fit::result<Errno, Container> create_container(const Config& config);

fit::result<zx_status_t, fbl::RefPtr<starnix::FsContext>> create_fs_context(
    const fbl::RefPtr<starnix::Kernel>& kernel, const Config& config);

}  // namespace starnix_kernel_runner

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_CONTAINER_H_
