// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_loader/loader.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/io.h>
#include <lib/fit/defer.h>
#include <lib/ld/abi.h>
#include <lib/ld/remote-abi-heap.h>
#include <lib/ld/remote-abi-stub.h>
#include <lib/ld/remote-abi.h>

#include <fbl/unique_fd.h>

#include "src/devices/bin/driver_loader/diagnostics.h"
#include "src/devices/lib/log/log.h"

namespace fio = fuchsia_io;

namespace {

zx::result<zx::vmo> GetStubLdVmo() {
  constexpr char kPath[] = "/pkg/lib/ld-stub.so";
  const fio::wire::OpenFlags kFlags = fio::wire::OpenFlags::kNotDirectory |
                                      fio::wire::OpenFlags::kRightReadable |
                                      fio::wire::OpenFlags::kRightExecutable;
  fbl::unique_fd fd;
  zx_status_t status =
      fdio_open_fd(kPath, static_cast<uint32_t>(kFlags), fd.reset_and_get_address());
  if (status != ZX_OK) {
    return zx::error(status);
  }
  zx::vmo vmo;
  status = fdio_get_vmo_exec(fd.get(), vmo.reset_and_get_address());
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(vmo));
}

}  // namespace

namespace driver_loader {

// static
std::unique_ptr<Loader> Loader::Create() {
  zx::result<zx::vmo> stub_ld_vmo = GetStubLdVmo();
  if (stub_ld_vmo.is_error()) {
    LOGF(ERROR, "Failed to get stub ld vmo for loader: %s", stub_ld_vmo.status_string());
    return nullptr;
  }
  auto diag = MakeDiagnostics();
  return std::unique_ptr<Loader>(new Loader(diag, std::move(*stub_ld_vmo)));
}

zx_status_t Loader::Start(zx::process process, zx::thread thread, zx::vmar root_vmar,
                          zx::vmo exec_vmo, zx::vmo vdso_vmo,
                          fidl::ClientEnd<fuchsia_io::Directory> lib_dir) {
  auto diag = MakeDiagnostics();

  Linker linker;
  linker.set_abi_stub(remote_abi_stub_);
  if (!linker.abi_stub()) {
    return ZX_ERR_INTERNAL;
  }

  Linker::Module::DecodedPtr decoded_executable =
      Linker::Module::Decoded::Create(diag, std::move(exec_vmo), kPageSize);
  if (!decoded_executable) {
    return ZX_ERR_INVALID_ARGS;
  }
  Linker::Module::DecodedPtr decoded_vdso =
      Linker::Module::Decoded::Create(diag, std::move(vdso_vmo), kPageSize);
  if (!decoded_vdso) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto init_result = linker.Init(diag,
                                 {Linker::Executable(std::move(decoded_executable)),
                                  Linker::Implicit(std::move(decoded_vdso))},
                                 GetDepFunction(diag, std::move(lib_dir)));
  if (!init_result) {
    return ZX_ERR_INTERNAL;
  }

  // We expect this to be equal to the size of the initial_modules list passed to |linker.Init|.
  ZX_ASSERT_MSG(init_result->size() == 2u,
                "Linker.Init returned an unexpected number of elements, want %u got %lu", 2u,
                init_result->size());

  // Allocate new child vmars.
  if (!linker.Allocate(diag, root_vmar.borrow())) {
    return ZX_ERR_INTERNAL;
  }

  const Linker::Module& loaded_vdso = *init_result->back();
  struct ProcessState::ProcessStartArgs process_start_args = {
      .entry = linker.main_entry(),
      .vdso_base = loaded_vdso.module().vaddr_start(),
      .stack_size = linker.main_stack_size(),
  };

  // Apply relocations to segment VMOs.
  if (!linker.Relocate(diag)) {
    return ZX_ERR_INTERNAL;
  }

  if (diag.errors() != 0) {
    LOGF(ERROR, "Linker diagnostics reported %u errors, skipping loading", diag.errors());
    return ZX_ERR_INTERNAL;
  }

  // Load the segments into the allocated vmars.
  if (!linker.Load(diag)) {
    return ZX_ERR_INTERNAL;
  }

  if (diag.errors() != 0) {
    LOGF(ERROR, "Linker diagnostics reported %u errors, skipping linker.Commit", diag.errors());
    return ZX_ERR_INTERNAL;
  }

  // Commit the VMARs and mappings.
  linker.Commit();

  // Start the process.
  auto process_state = ProcessState::Create(std::move(process), std::move(thread),
                                            std::move(root_vmar), std::move(process_start_args));
  if (process_state.is_error()) {
    LOGF(ERROR, "Failed to create process state: %s", process_state.status_string());
    return process_state.status_value();
  }
  zx_status_t status = process_state->Start();
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to start process: %s", zx_status_get_string(status));
    return status;
  }
  started_processes_.push_back(std::move(*process_state));
  return ZX_OK;
}

zx::result<zx::vmo> Loader::GetDepVmo(fidl::UnownedClientEnd<fuchsia_io::Directory> lib_dir,
                                      const char* libname) {
  auto [client_end, server_end] = fidl::Endpoints<fio::File>::Create();
  zx_status_t status = fdio_open_at(
      lib_dir.channel()->get(), libname,
      static_cast<uint32_t>(fio::OpenFlags::kRightReadable | fio::OpenFlags::kRightExecutable),
      server_end.TakeChannel().release());
  if (status != ZX_OK) {
    return zx::error(status);
  }
  fidl::SyncClient file(std::move(client_end));
  auto result = file->GetBackingMemory(fio::VmoFlags::kRead | fio::VmoFlags::kExecute |
                                       fio::VmoFlags::kPrivateClone);
  if (result.is_error()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return zx::ok(std::move(result->vmo()));
}

// static
zx::result<std::unique_ptr<Loader::ProcessState>> Loader::ProcessState::Create(
    zx::process process, zx::thread thread, zx::vmar root_vmar, const ProcessStartArgs& args) {
  auto process_state = std::unique_ptr<ProcessState>(new ProcessState(
      std::move(process), std::move(thread), std::move(root_vmar), std::move(args)));
  zx_status_t status = process_state->AllocateStack();
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(process_state));
}

// TODO(https://fxbug.dev/349913885): this should be replaced by a call to a helper library
// once available.
zx_status_t Loader::ProcessState::AllocateStack() {
  zx::vmo stack_vmo;

  size_t bootstrap_stack_size = process_start_args_.stack_size.value_or(kDefaultStackSize);

  const size_t guard_size = Loader::kPageSize;
  const size_t stack_vmo_size = (bootstrap_stack_size + Loader::kPageSize - 1) & -Loader::kPageSize;
  const size_t stack_vmar_size = stack_vmo_size + guard_size;

  zx_status_t status = zx::vmo::create(stack_vmo_size, 0, &stack_vmo);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to create vmo for stack: %s", zx_status_get_string(status));
    return status;
  }

  zx::vmar stack_vmar;
  uintptr_t stack_vmar_base;
  status = root_vmar_.allocate(ZX_VM_CAN_MAP_SPECIFIC | ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE, 0,
                               stack_vmar_size, &stack_vmar, &stack_vmar_base);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to allocate child vmar for stack: %s", zx_status_get_string(status));
    return status;
  }

  zx_vaddr_t stack_base;
  status = stack_vmar.map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_SPECIFIC | ZX_VM_ALLOW_FAULTS,
                          guard_size, stack_vmo, 0, stack_vmo_size, &stack_base);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to map stack vmar: %s", zx_status_get_string(status));
    return status;
  }

  stack_ = std::move(stack_vmo);
  initial_stack_pointer_ = elfldltl::AbiTraits<>::InitialStackPointer(stack_base, stack_vmo_size);
  return ZX_OK;
}

zx_status_t Loader::ProcessState::Start() {
  zx::channel bootstrap_receiver = zx::channel(ZX_HANDLE_INVALID);
  return process_.start(thread_, process_start_args_.entry, initial_stack_pointer_,
                        std::move(bootstrap_receiver), process_start_args_.vdso_base);
}

}  // namespace driver_loader
