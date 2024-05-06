// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_INCLUDE_LIB_MISTOS_USERLOADER_START_H_
#define ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_INCLUDE_LIB_MISTOS_USERLOADER_START_H_

#include <lib/mistos/userloader/userloader.h>
#include <lib/mistos/zbi_parser/bootfs.h>
#include <lib/mistos/zbi_parser/option.h>
#include <lib/mistos/zx/debuglog.h>
#include <lib/mistos/zx/process.h>
#include <lib/mistos/zx/thread.h>
#include <lib/mistos/zx/vmar.h>

#include <fbl/string.h>
#include <fbl/vector.h>
#include <ktl/string_view.h>
#include <object/handle.h>

struct ChildContext {
  ChildContext() = default;
  ChildContext(ChildContext&&) = default;
  ~ChildContext() = default;

  // Process creation handles
  zx::process process;
  zx::vmar vmar;
  zx::thread thread;
};

// Set of resources created in userboot.
struct Resources {
  // Needed for properly implementing the epilogue.
  zx::resource power;

  // Needed for vending executable memory from bootfs.
  zx::resource vmex;
};

ktl::array<zx_handle_t, userloader::kHandleCount> ExtractHandles(
    ktl::array<Handle*, userloader::kHandleCount>);

Resources CreateResources(const zx::debuglog& log,
                          ktl::span<const zx_handle_t, userloader::kHandleCount> handles);

ChildContext CreateChildContext(const zx::debuglog& log, ktl::string_view name);
zx_status_t StartChildProcess(const zx::debuglog& log,
                              const zbi_parser::Options::ProgramInfo& elf_entry,
                              ChildContext& child, zbi_parser::Bootfs& bootfs,
                              const fbl::Vector<fbl::String>& argv,
                              const fbl::Vector<fbl::String>& envp);
int64_t WaitForProcessExit(const zx::debuglog& log, const zbi_parser::Options::ProgramInfo& entry,
                           const ChildContext& child);

#endif  // ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_INCLUDE_LIB_MISTOS_USERLOADER_START_H_
