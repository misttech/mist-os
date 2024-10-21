// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_KERNEL_THREADS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_KERNEL_THREADS_H_

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/util/onecell.h>
#include <lib/mistos/util/weak_wrapper.h>

namespace starnix {

class ThreadGroup;
class KernelThreads;
class SystemTask {
 private:
  /// The system task is bound to the kernel main thread. `Fragile` ensures a runtime crash if it
  /// is accessed from any other thread.
  // TODO (Herrera) : Implement equivalent of `Fragile` for `CurrentTask`.
  ktl::optional<CurrentTask> system_task_;

  /// The system `ThreadGroup` is accessible from everywhere.
  util::WeakPtr<ThreadGroup> system_thread_group_;

 public:
  friend KernelThreads;

  explicit SystemTask(CurrentTask system_task);
};

class Kernel;

class KernelThreads {
 private:
  /// Information about the main system task that is bound to the kernel main thread.
  OnceCell<SystemTask> system_task_;

  /// A weak reference to the kernel owning this struct.
  util::WeakPtr<Kernel> kernel_;

  // impl KernelThreads
 public:
  /// Create a KernelThreads object for the given Kernel.
  ///
  /// Must be called in the initial Starnix process on a thread with an async executor. This
  /// function captures the async executor for this thread for use with spawned futures.
  ///
  /// Used during kernel boot.
  static KernelThreads New(util::WeakPtr<Kernel> kernel);

  /// Initialize this object with the system task that will be used for spawned threads.
  ///
  /// This function must be called before this object is used to spawn threads.
  fit::result<Errno> Init(CurrentTask system_task);

  // Access the `CurrentTask` for the kernel main thread.
  ///
  /// This function can only be called from the kernel main thread itself.
  CurrentTask& system_task();

  // C++
  ~KernelThreads();

 private:
  explicit KernelThreads(util::WeakPtr<Kernel> kernel) : kernel_(std::move(kernel)) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_KERNEL_THREADS_H_
