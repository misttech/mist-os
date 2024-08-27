// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_RUNNER_CONTAINER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_RUNNER_CONTAINER_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/runner/config.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/vfs/forward.h>
#include <lib/mistos/starnix_uapi/errors.h>

#include <fbl/ref_ptr.h>

namespace starnix {

// A temporary wrapper struct that contains both a `Config` for the container, as well as optional
// handles for the container's component controller and `/pkg` directory.
//
// When using structured_config, the `component_controller` handle will not be set. When all
// containers are run as components, by starnix_runner, the `component_controller` will always
// exist.
struct ConfigWrapper {
  Config config;

  // deref
  const Config* operator->() const { return &config; }
};

class Kernel;

struct Container {
  /// The `Kernel` object that is associated with the container.
  fbl::RefPtr<Kernel> kernel;

  /// Inspect node holding information about the state of the container.
  //_node: inspect::Node,

  /// Until negative trait bound are implemented, using `*mut u8` to prevent transferring
  /// Container across threads.
  //_thread_bound: std::marker::PhantomData<*mut u8>,
};

fit::result<Errno, Container> create_container(const ConfigWrapper& config);

fit::result<zx_status_t, fbl::RefPtr<FsContext>> create_fs_context(
    const fbl::RefPtr<Kernel>& kernel, const ConfigWrapper& config);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_RUNNER_CONTAINER_H_
