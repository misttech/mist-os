// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_REMOTE_PROCESS_TESTS_H_
#define LIB_LD_TEST_LD_REMOTE_PROCESS_TESTS_H_

#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/elfldltl/testing/get-test-data.h>
#include <lib/ld/remote-abi-heap.h>
#include <lib/ld/remote-abi-stub.h>
#include <lib/ld/remote-abi.h>
#include <lib/ld/remote-dynamic-linker.h>
#include <lib/ld/remote-load-module.h>
#include <lib/ld/testing/mock-loader-service.h>
#include <lib/ld/testing/test-vmo.h>
#include <lib/zx/channel.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>

#include <initializer_list>
#include <optional>
#include <string_view>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "ld-load-zircon-process-tests-base.h"

namespace ld::testing {

class LdRemoteProcessTests : public ::testing::Test, public LdLoadZirconProcessTestsBase {
 public:
  static constexpr bool kCanCollectLog = false;

  inline static const size_t kPageSize = zx_system_get_page_size();

  LdRemoteProcessTests();

  ~LdRemoteProcessTests() override;

  void SetUp() override;

  void Init(std::initializer_list<std::string_view> args = {},
            std::initializer_list<std::string_view> env = {});

  void Needed(std::initializer_list<std::string_view> names) { mock_loader_.Needed(names); }

  void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs) {
    mock_loader_.Needed(name_found_pairs);
  }

  void VerifyAndClearNeeded() { mock_loader_.VerifyAndClearExpectations(); }

  void Load(std::string_view executable_name) {
    elfldltl::testing::ExpectOkDiagnostics diag;
    ASSERT_NO_FATAL_FAILURE(Load(diag, executable_name, false));
  }

  int64_t Run();

  template <class... Reports>
  void LoadAndFail(std::string_view name, elfldltl::testing::ExpectedErrorList<Reports...> diag) {
    ASSERT_NO_FATAL_FAILURE(Load(diag, name, true));
    ASSERT_NO_FATAL_FAILURE(ExpectLog(""));
  }

  zx::vmo TakeStubLdVmo() { return std::move(stub_ld_vmo_); }

 protected:
  using Linker = RemoteDynamicLinker<>;
  using RemoteModule = RemoteLoadModule<>;

  // This returns a closure usable as the `get_dep` function for calling
  // ld::RuntimeDynamicLinker::Init.  It captures the Diagnostics reference and
  // the this pointer; when called it uses LoadObject on the MockLoaderService
  // to find files (or not) according to the Needed calls priming the expected
  // sequence of names.
  template <class Diagnostics>
  auto GetDepFunction(Diagnostics& diag) {
    return [this, &diag](const RemoteModule::Soname& soname) -> Linker::GetDepResult {
      RemoteModule::Decoded::Ptr decoded;
      auto vmo = mock_loader_.LoadObject(soname.str());
      if (vmo.is_ok()) [[likely]] {
        // If it returned fit::ok(zx::vmo{}), keep going without this module.
        if (*vmo) [[likely]] {
          decoded = RemoteModule::Decoded::Create(diag, *std::move(vmo), kPageSize);
        }
      } else if (vmo.error_value() == ZX_ERR_NOT_FOUND
                     ? !diag.MissingDependency(soname.str())
                     : !diag.SystemError("cannot open dependency ", soname.str(), ": ",
                                         elfldltl::ZirconError{vmo.error_value()})) {
        // Diagnostics said to bail out now.
        return std::nullopt;
      }
      return std::move(decoded);
    };
  }

  const zx::vmar& root_vmar() { return root_vmar_; }

  void set_entry(uintptr_t entry) { entry_ = entry; }

  void set_vdso_base(uintptr_t vdso_base) { vdso_base_ = vdso_base; }

  void set_stack_size(std::optional<size_t> stack_size) { stack_size_ = stack_size; }

 private:
  template <class Diagnostics>
  void Load(Diagnostics& diag, std::string_view executable_name, bool should_fail) {
    Linker linker;

    // Decode the main executable.
    zx::vmo vmo;
    ASSERT_NO_FATAL_FAILURE(vmo = GetExecutableVmo(executable_name));
    std::array initial_modules = {Linker::InitModule{
        .decoded_module = RemoteModule::Decoded::Create(diag, std::move(vmo), kPageSize),
    }};
    ASSERT_TRUE(initial_modules.front().decoded_module);

    // Pre-decode the vDSO.
    zx::vmo vdso_vmo;
    zx_status_t status = GetVdsoVmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vdso_vmo);
    ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);

    std::array predecoded_modules = {Linker::PredecodedModule{
        .decoded_module = RemoteModule::Decoded::Create(diag, std::move(vdso_vmo), kPageSize),
    }};

    // Acquire the layout details from the stub.  The same values collected
    // here can be reused along with the decoded RemoteLoadModule for the stub
    // for creating and populating the RemoteLoadModule for the passive ABI of
    // any number of separate dynamic linking domains in however many
    // processes.
    RemoteAbiStub<>::Ptr abi_stub = RemoteAbiStub<>::Create(diag, TakeStubLdVmo(), kPageSize);
    EXPECT_TRUE(abi_stub);
    linker.set_abi_stub(abi_stub);

    // First just decode all the modules: the executable and dependencies.
    auto init_result =
        linker.Init(diag, initial_modules, GetDepFunction(diag), std::move(predecoded_modules));
    ASSERT_TRUE(init_result);
    auto& modules = linker.modules();
    ASSERT_FALSE(modules.empty());

    // If not all modules could be decoded, don't bother with relocation to
    // diagnose symbol resolution errors since many are likely without all the
    // modules there and they are unlikely to add any helpful information
    // beyond the diagnostic about decoding problems (e.g. missing modules).
    // This is consistent with the startup dynamic linker, which reports all
    // the decode / load problems it can before bailing out if there were any.
    // In a general library implementation, it will be up to the caller of the
    // library to decide whether to attempt later stages with an incomplete
    // module list.  The library code endeavors to ensure it will be safe to
    // make the attempt with missing or partially-decoded modules in the list.
    if (!linker.AllModulesValid()) {
      // Whatever the failures were have already been diagnosed.
      // This isn't a test failure in LoadAndFail tests.
      EXPECT_EQ(this->HasFailure(), !should_fail);
      return;
    }

    // Choose load addresses.
    EXPECT_TRUE(linker.Allocate(diag, root_vmar().borrow()));

    // Use the executable's entry point at its runtime load address.
    set_entry(linker.main_entry());

    // Record any stack size request from the executable's PT_GNU_STACK.
    set_stack_size(linker.main_stack_size());

    // Locate the loaded vDSO to pass its base pointer to the test process.
    const RemoteModule& loaded_vdso = modules[init_result->front()];
    set_vdso_base(loaded_vdso.module().vaddr_start());

    // Apply relocations to segment VMOs.
    EXPECT_TRUE(linker.Relocate(diag));

    ASSERT_EQ(IsExpectOkDiagnostics(diag), !should_fail);
    if (should_fail) {
      // Whatever the failures were have already been diagnosed.  This isn't a
      // test failure in LoadAndFail tests.  But don't really keep going past
      // this point.  As the RelocateModules API comment suggests, it often
      // makes sense to go this far despite prior errors just to maximize all
      // the errors reported, e.g. all the undefined symbols and not just the
      // first one.  For the library API, the caller is free to proceed further
      // if they choose, but that's not consistent with the startup dynamic
      // linker.  These tests expect the startup dynamic linker's behavior,
      // which is to report all decoding / loading failures, then bail if there
      // were any; then report all relocation failures, then bail if there were
      // any.
      EXPECT_FALSE(this->HasFailure());
      return;
    }

    for (size_t i = 0; i < modules.size(); ++i) {
      EXPECT_EQ(modules[i].module().symbolizer_modid, i);
    }

    // Finally, all the VMO contents are in place to be mapped into the
    // process.
    ASSERT_TRUE(linker.Load(diag));

    // Any failure before here would destroy all the VMARs when linker goes out
    // of scope.  From here the mappings will stick in the process.
    linker.Commit();
  }

  uintptr_t entry_ = 0;
  uintptr_t vdso_base_ = 0;
  std::optional<size_t> stack_size_;
  zx::vmo stub_ld_vmo_;
  zx::vmar root_vmar_;
  zx::thread thread_;
  MockLoaderServiceForTest mock_loader_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_REMOTE_PROCESS_TESTS_H_
