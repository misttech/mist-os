// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_CONTAINER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_CONTAINER_H_

#include <lib/fit/result.h>
#include <lib/mistos/util/error.h>

#include <fbl/ref_ptr.h>

namespace starnix {
class Kernel;
class FsContext;
}  // namespace starnix

namespace starnix_kernel_runner {

struct Config {
  /// The features enabled for this container.
  fbl::Vector<ktl::string_view> features;

  /// The command line for the initial process for this container.
  fbl::Vector<ktl::string_view> init;

  /// The command line for the kernel.
  ktl::string_view kernel_cmdline;

  /// The specifications for the file system mounts for this container.
  fbl::Vector<ktl::string_view> mounts;

  /// The resource limits to apply to this container.
  fbl::Vector<ktl::string_view> rlimits;

  /// The name of this container.
  ktl::string_view name;

  /// The path that the container will wait until exists before considering itself to have started.
  ktl::string_view startup_file_path;
};

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

fit::result<mtl::Error, Container> create_container(const Config& config);

fit::result<mtl::Error, fbl::RefPtr<starnix::FsContext>> create_fs_context(
    const fbl::RefPtr<starnix::Kernel>& kernel, const Config& config);

}  // namespace starnix_kernel_runner

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_CONTAINER_H_
