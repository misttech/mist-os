// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_LOADER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_LOADER_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/starnix/kernel/vfs/forward.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/zx/vmo.h>

#include <fbl/string.h>
#include <fbl/vector.h>
#include <ktl/array.h>

#include "lib/mistos/zx/arc.h"

constexpr size_t HASH_BANG_SIZE = 2;
constexpr ktl::array<char, HASH_BANG_SIZE> HASH_BANG = {'#', '!'};
constexpr size_t MAX_RECURSION_DEPTH = 5;

using namespace starnix_uapi;

namespace starnix {

struct ThreadStartInfo {
  UserAddress entry;
  UserAddress stack;
};

// Holds a resolved ELF interpreter VMO.
struct ResolvedInterpElf {
  /// A file handle to the resolved ELF interpreter.
  FileHandle file;
  // A VMO to the resolved ELF interpreter.
  zx::ArcVmo vmo;
  /// Exec/write lock.
  // file_write_guard: FileWriteGuardRef,
};

// Holds a resolved ELF VMO and associated parameters necessary for an execve call.
struct ResolvedElf {
  /// A file handle to the resolved ELF executable.
  FileHandle file;
  // A VMO to the resolved ELF executable.
  zx::ArcVmo vmo;
  /// An ELF interpreter, if specified in the ELF executable header.
  ktl::optional<ResolvedInterpElf> interp;
  /// Arguments to be passed to the new process.
  fbl::Vector<fbl::String> argv;
  /// The environment to initialize for the new process.
  fbl::Vector<fbl::String> environ;
  /// The SELinux state for the new process. None if SELinux is disabled.
  // pub selinux_state: Option<SeLinuxResolvedElfState>,
  /// Exec/write lock.
  // pub file_write_guard: FileWriteGuardRef,
};

// Resolves a file into a validated executable ELF, following script interpreters to a fixed
// recursion depth. `argv` may change due to script interpreter logic.
fit::result<Errno, ResolvedElf> resolve_executable(
    const CurrentTask& current_task, const FileHandle& file, const fbl::String& path,
    const fbl::Vector<fbl::String>& argv,
    const fbl::Vector<fbl::String>& environ /*,selinux_state: Option<SeLinuxResolvedElfState>*/);

// Loads a resolved ELF into memory, along with an interpreter if one is defined, and initializes
// the stack.
fit::result<Errno, ThreadStartInfo> load_executable(const CurrentTask& current_task,
                                                    const ResolvedElf& resolved_elf,
                                                    const fbl::String& original_path);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_LOADER_H_
