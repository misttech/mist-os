// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_EXECUTOR_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_EXECUTOR_H_

#include <object/vm_address_region_dispatcher.h>

namespace zx {

// An Executor encapsulates the kernel state necessary to implement the Zircon system calls. It
// depends on an interface from the kernel below it, presenting primitives like threads and wait
// queues. It presents an interface to the system call implementations.
//
// The goals of factoring this into such a layer include:
//
// - The ability to test code in this layer separately from low-level kernel implementation details,
//   and from the syscall mechanism. This includes correctness as well as performance tests.
//
// - Centralize resource management in order to make progress on things like not reporting
//   ZX_ERR_NO_MEMORY when creating a zx::event, or reporting bad handle faults.
//
class Executor {
 public:
  void Init();

  const fbl::RefPtr<VmAddressRegionDispatcher>& GetKernelVmarDispatcher() { return kernel_vmar_; }

  Handle* GetKernelVmarHandle() { return kernel_vmar_handle_.get(); }

 private:
  // All jobs and processes of this Executor are rooted at this job.
  fbl::RefPtr<VmAddressRegionDispatcher> kernel_vmar_;
  HandleOwner kernel_vmar_handle_;
};

Handle* GetKernelVmarHandle();

}  // namespace zx

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_EXECUTOR_H_
