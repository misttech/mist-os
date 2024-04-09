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
#include <lib/ld/testing/test-vmo.h>
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

  LdRemoteProcessTests();

  ~LdRemoteProcessTests() override;

  void Init(std::initializer_list<std::string_view> args = {},
            std::initializer_list<std::string_view> env = {});

  void Needed(std::initializer_list<std::string_view> names);

  void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs);

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

 protected:
  const zx::vmar& root_vmar() { return root_vmar_; }

  void set_entry(uintptr_t entry) { entry_ = entry; }

  void set_vdso_base(uintptr_t vdso_base) { vdso_base_ = vdso_base; }

  void set_stack_size(std::optional<size_t> stack_size) { stack_size_ = stack_size; }

 private:
  class MockLoader;

  using Linker = RemoteDynamicLinker<>;
  using RemoteModule = RemoteLoadModule<>;

  static zx::vmo GetTestVmo(std::string_view path);

  zx::vmo GetDepVmo(const RemoteModule::Soname& soname);

  template <class Diagnostics>
  void Load(Diagnostics& diag, std::string_view executable_name, bool should_fail) {
    const size_t page_size = zx_system_get_page_size();
    Linker linker;

    // Decode the main executable.
    const std::string executable_path = std::filesystem::path("test") / "bin" / executable_name;
    zx::vmo vmo;
    ASSERT_NO_FATAL_FAILURE(vmo = elfldltl::testing::GetTestLibVmo(executable_path));
    std::array initial_modules = {Linker::InitModule{
        .decoded_module = RemoteModule::Decoded::Create(diag, std::move(vmo), page_size),
    }};
    ASSERT_TRUE(initial_modules.front().decoded_module);

    // Pre-decode the vDSO.
    zx::vmo stub_ld_vmo;
    ASSERT_NO_FATAL_FAILURE(stub_ld_vmo = elfldltl::testing::GetTestLibVmo("ld-stub.so"));

    zx::vmo vdso_vmo;
    zx_status_t status = GetVdsoVmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vdso_vmo);
    ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);

    std::array predecoded_modules = {Linker::PredecodedModule{
        .decoded_module = RemoteModule::Decoded::Create(diag, std::move(vdso_vmo), page_size),
    }};

    // Acquire the layout details from the stub.  The same values collected
    // here can be reused along with the decoded RemoteLoadModule for the stub
    // for creating and populating the RemoteLoadModule for the passive ABI of
    // any number of separate dynamic linking domains in however many
    // processes.
    RemoteAbiStub<>::Ptr abi_stub =
        RemoteAbiStub<>::Create(diag, std::move(stub_ld_vmo), page_size);
    EXPECT_TRUE(abi_stub);
    EXPECT_GE(abi_stub->data_size(), sizeof(ld::abi::Abi<>) + sizeof(elfldltl::Elf<>::RDebug<>));
    EXPECT_LT(abi_stub->data_size(), page_size);
    EXPECT_LE(abi_stub->abi_offset(), abi_stub->data_size() - sizeof(ld::abi::Abi<>));
    EXPECT_LE(abi_stub->rdebug_offset(), abi_stub->data_size() - sizeof(elfldltl::Elf<>::RDebug<>));
    EXPECT_NE(abi_stub->rdebug_offset(), abi_stub->abi_offset())
        << "with data_size() " << abi_stub->data_size();

    linker.set_abi_stub(abi_stub);

    // Verify that the TLSDESC entry points were found in the stub and that
    // their addresses pass some basic smell tests.
    std::set<elfldltl::Elf<>::size_type> tlsdesc_entrypoints;
    const auto segment_is_executable = [](const auto& segment) -> bool {
      return segment.executable();
    };
    const RemoteModule::Decoded& stub_module = *abi_stub->decoded_module();
    for (const elfldltl::Elf<>::size_type entry : abi_stub->tlsdesc_runtime()) {
      // Must be nonzero.
      EXPECT_NE(entry, 0u);

      // Must lie within the module bounds.
      EXPECT_GT(entry, stub_module.load_info().vaddr_start());
      EXPECT_LT(entry - stub_module.load_info().vaddr_start(),
                stub_module.load_info().vaddr_size());

      // Must be inside an executable segment.
      auto segment = stub_module.load_info().FindSegment(entry);
      ASSERT_NE(segment, stub_module.load_info().segments().end());
      EXPECT_TRUE(std::visit(segment_is_executable, *segment));

      // Must be unique.
      auto [it, inserted] = tlsdesc_entrypoints.insert(entry);
      EXPECT_TRUE(inserted) << "duplicate entry point " << entry;
    }
    EXPECT_EQ(tlsdesc_entrypoints.size(), kTlsdescRuntimeCount);

    // First just decode all the modules: the executable and dependencies.
    auto get_dep = [this, page_size,
                    &diag](const RemoteModule::Soname& soname) -> Linker::GetDepResult {
      RemoteModule::Decoded::Ptr decoded;
      if (zx::vmo vmo = GetDepVmo(soname)) [[likely]] {
        decoded = RemoteModule::Decoded::Create(diag, std::move(vmo), page_size);
      } else if (!diag.MissingDependency(soname.str())) {
        return std::nullopt;
      }
      return std::move(decoded);
    };
    auto init_result = linker.Init(diag, initial_modules, get_dep, std::move(predecoded_modules));
    ASSERT_TRUE(init_result);
    auto& modules = linker.modules();
    ASSERT_FALSE(modules.empty());

    // Check the loaded-by pointers.
    EXPECT_FALSE(modules.front().loaded_by_modid())
        << "executable loaded by " << modules[*modules.front().loaded_by_modid()].name();
    {
      auto next_module = std::next(modules.begin());
      auto loaded_by_name = [next_module, &modules]() -> std::string_view {
        if (next_module->loaded_by_modid()) {
          return modules[*next_module->loaded_by_modid()].name().str();
        }
        return "<none>";
      };
      if (next_module != modules.end() && next_module->HasModule() &&
          next_module->module().symbols_visible) {
        // The second module must be a direct dependency of the executable.
        EXPECT_THAT(next_module->loaded_by_modid(), ::testing::Optional(0u))
            << " second module " << next_module->name().str() << " loaded by " << loaded_by_name();
      }
      for (; next_module != modules.end(); ++next_module) {
        if (!next_module->HasModule()) {
          continue;
        }
        if (next_module->module().symbols_visible) {
          // This module wouldn't be here if it wasn't loaded by someone.
          EXPECT_NE(next_module->loaded_by_modid(), std::nullopt)
              << "visible module " << next_module->name().str() << " loaded by "
              << loaded_by_name();
        } else {
          // A predecoded module was not referenced, so it's loaded by no-one.
          EXPECT_EQ(next_module->loaded_by_modid(), std::nullopt)
              << "invisible module " << next_module->name().str() << " loaded by "
              << loaded_by_name();
        }
      }
    }

    const RemoteModule& loaded_stub = linker.abi_stub_module();
    if (!loaded_stub.HasModule()) {
      ASSERT_TRUE(this->HasFailure());
      return;
    }

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

    // Now that load addresses have been chosen, populate the remote ABI data.
    auto abi_result = std::move(linker.remote_abi()).Finish(diag, loaded_stub, modules);
    EXPECT_TRUE(abi_result.is_ok()) << abi_result.status_string();

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
  zx::vmar root_vmar_;
  zx::thread thread_;
  std::unique_ptr<MockLoader> mock_loader_;
};
}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_REMOTE_PROCESS_TESTS_H_
