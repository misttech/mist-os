// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_STARTUP_IN_PROCESS_TESTS_POSIX_H_
#define LIB_LD_TEST_LD_STARTUP_IN_PROCESS_TESTS_POSIX_H_

#include <lib/elfldltl/mmap-loader.h>
#include <lib/stdcompat/span.h>

#include <cstddef>
#include <cstdint>
#include <initializer_list>
#include <string>
#include <string_view>
#include <tuple>

#include <gtest/gtest.h>

// The in-process here work by doing ELF loading approximately as the system
// program loader would, but into this process that's running the test.  Once
// the dynamic linker has been loaded, the InProcessTestLaunch object knows how
// its entry point wants to be called.  It's responsible for collecting the
// information to be passed to the dynamic linker, and then doing the call into
// its entry point to emulate what it would expect from the program loader
// starting an initial thread.

namespace ld::testing {

// On POSIX-like systems this means a canonical stack setup that transfers
// arguments, environment, and a set of integer key-value pairs called the
// auxiliary vector (auxv) that carries values important for bootstrapping.
class InProcessTestLaunch {
 public:
  // The loaded code is just writing to STDERR_FILENO in the same process.
  // There's no way to install e.g. a pipe end as STDERR_FILENO for the loaded
  // code without also hijacking stderr for the test harness itself, which
  // seems a bit dodgy even if the original file descriptor were saved and
  // dup2'd back after the test succeeds.  In the long run, most cases where
  // the real dynamic linker would emit any diagnostics are when it would then
  // crash the process, so those cases will only get tested via spawning a new
  // process, not in-process tests.
  static constexpr bool kHasLog = false;

  void Init(std::initializer_list<std::string_view> args = {});

  // No arguments are needed for MakeLoader().
  constexpr std::tuple<> LoaderArgs() { return {}; }

  // This is called after the dynamic linker has been loaded.  Save the loader
  // object so it gets destroyed when this InProcessTestLaunch object is
  // destroyed.  That will clean up the mappings it made.
  void AfterLoad(elfldltl::MmapLoader loader) { loader_ = std::move(loader); }

  template <class Test>
  void SendExecutable(std::string_view name, Test& test) {
    ASSERT_TRUE(auxv_);  // Called after Init calls PopulateStack.
    std::optional<typename Test::LoadResult> exec;
    ASSERT_NO_FATAL_FAILURE(test.Load(name, exec));

    // Set AT_PHDR and AT_PHNUM for where the phdrs were loaded.
    cpp20::span phdrs = exec->phdrs.get();
    exec->info.VisitSegments([load_bias = exec->loader.load_bias(), offset = exec->phoff(),
                              filesz = phdrs.size_bytes(), this](const auto& segment) {
      return OnExecutableSegment(load_bias, offset, filesz, segment.vaddr(), segment.offset(),
                                 segment.filesz());
    });
    FinishSendExecutable(phdrs.size(), exec->entry, std::move(exec->loader));
  }

  int Call(uintptr_t entry);

  std::string CollectLog() { return {}; }

  ~InProcessTestLaunch();

 private:
  struct AuxvBlock;

  bool OnExecutableSegment(uintptr_t load_bias, uintptr_t phoff, size_t phdrs_size_bytes,
                           uintptr_t vaddr, uintptr_t offset, size_t filesz);

  void FinishSendExecutable(size_t phnum, uintptr_t entry, elfldltl::MmapLoader loader);

  void AllocateStack();

  void PopulateStack(std::initializer_list<std::string_view> argv,
                     std::initializer_list<std::string_view> envp);

  elfldltl::MmapLoader loader_, exec_loader_;
  void* stack_ = nullptr;
  void* sp_ = nullptr;
  AuxvBlock* auxv_ = nullptr;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_STARTUP_IN_PROCESS_TESTS_POSIX_H_
