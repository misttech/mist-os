// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_LOADER_LOADER_H_
#define SRC_DEVICES_BIN_DRIVER_LOADER_LOADER_H_

#include <lib/ld/remote-dynamic-linker.h>
#include <lib/ld/remote-load-module.h>
#include <lib/zircon-internal/default_stack_size.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>

#include <memory>
#include <optional>

#include <fbl/intrusive_double_list.h>

#include "src/devices/bin/driver_loader/diagnostics.h"

namespace driver_loader {

class Loader {
 public:
  static std::unique_ptr<Loader> Create();

  // Loads the executable |exec| with |vdso| into |process|, and begins the execution on |thread|.
  zx_status_t Start(zx::process process, zx::thread thread, zx::vmar root_vmar, zx::vmo exec,
                    zx::vmo vdso);

 private:
  using Linker = ld::RemoteDynamicLinker<>;
  using RemoteModule = ld::RemoteLoadModule<>;

  inline static const size_t kPageSize = zx_system_get_page_size();
  inline static constexpr size_t kDefaultStackSize = ZIRCON_DEFAULT_STACK_SIZE;

  // Stores the state for a started process.
  class ProcessState : public fbl::DoublyLinkedListable<std::unique_ptr<ProcessState>> {
   public:
    struct ProcessStartArgs {
      // Entry point for the thread.
      uintptr_t entry = 0;
      // Base pointer of the loaded vdso.
      uintptr_t vdso_base = 0;
      // Stack size request from the executable's PT_GNU_STACK, if any.
      std::optional<size_t> stack_size = std::nullopt;
    };

    static zx::result<std::unique_ptr<ProcessState>> Create(
        zx::process process, zx::thread thread, zx::vmar root_vmar,
        const ProcessStartArgs& process_start_args);

    // Begins execution of the process.
    zx_status_t Start();

   private:
    // Use |Create| instead.
    ProcessState(zx::process process, zx::thread thread, zx::vmar root_vmar,
                 const ProcessStartArgs& process_start_args)
        : process_(std::move(process)),
          thread_(std::move(thread)),
          root_vmar_(std::move(root_vmar)),
          process_start_args_(std::move(process_start_args)) {}

    // Allocates the stack for the initial thread of the process.
    zx_status_t AllocateStack();

    zx::process process_;
    zx::thread thread_;
    zx::vmar root_vmar_;

    ProcessStartArgs process_start_args_;

    // Stack for the initial thread allocated by |AllocateStack|.
    zx::vmo stack_;
    uintptr_t initial_stack_pointer_ = 0;
  };

  // Use |Create| instead.
  explicit Loader(Diagnostics& diag, zx::vmo stub_ld_vmo)
      : remote_abi_stub_(ld::RemoteAbiStub<>::Create(diag, std::move(stub_ld_vmo), kPageSize)) {}

  ld::RemoteAbiStub<>::Ptr remote_abi_stub_;

  fbl::DoublyLinkedList<std::unique_ptr<ProcessState>> started_processes_;
};

}  // namespace driver_loader

#endif  // SRC_DEVICES_BIN_DRIVER_LOADER_LOADER_H_
