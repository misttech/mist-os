// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_STARTUP_IN_PROCESS_TESTS_ZIRCON_H_
#define LIB_LD_TEST_LD_STARTUP_IN_PROCESS_TESTS_ZIRCON_H_

#include <lib/ld/testing/test-log-socket.h>
#include <lib/ld/testing/test-processargs.h>
#include <lib/zx/vmar.h>
#include <zircon/processargs.h>
#include <zircon/syscalls.h>

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

// On Fuchsia this means packing a message on the bootstrap channel.  The entry
// point receives the bootstrap channel (zx_handle_t) and the base address of
// the vDSO.
class InProcessTestLaunch {
 public:
  static constexpr bool kHasLog = true;

  // The dynamic linker gets loaded into this same test process, but it's given
  // a sub-VMAR to consider its "root" or allocation range so hopefully it will
  // confine its pointer references to that part of the address space.  The
  // dynamic linker doesn't necessarily clean up all its mappings--on success,
  // it leaves many mappings in place.  Test VMAR is always destroyed when the
  // InProcessTestLaunch object goes out of scope.
  static constexpr size_t kVmarSize = 1 << 30;

  void Init(std::initializer_list<std::string_view> args = {});

  // Arguments for calling MakeLoader(const zx::vmar&).
  auto LoaderArgs() const { return std::forward_as_tuple(test_vmar_); }

  // This is called after the dynamic linker is loaded.
  template <class Loader>
  void AfterLoad(Loader&& loader) {
    // The ends the useful lifetime of the loader object by extracting the VMAR
    // where it loaded the test image.  This VMAR handle doesn't need to be
    // saved here, since it's a sub-VMAR of the test_vmar_ that will be
    // destroyed when this InProcessTestLaunch object dies.
    zx::vmar load_image_vmar = std::move(loader).Commit();

    // Pass along that handle in the bootstrap message.
    procargs_.AddHandle(PA_VMAR_LOADED, std::move(load_image_vmar));
  }

  template <class Test>
  void SendExecutable(std::string_view name, Test& test) {
    SendExecutable(name);
  }

  ld::testing::TestProcessArgs& procargs() { return procargs_; }

  std::string CollectLog() { return std::move(log_).Read(); }

  int Call(uintptr_t entry);

  ~InProcessTestLaunch();

 private:
  using EntryFunction = int(zx_handle_t, void*);

  void SendExecutable(std::string_view name);

  static void* GetVdso();

  ld::testing::TestProcessArgs procargs_;
  ld::testing::TestLogSocket log_;
  zx::vmar test_vmar_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_STARTUP_IN_PROCESS_TESTS_ZIRCON_H_
