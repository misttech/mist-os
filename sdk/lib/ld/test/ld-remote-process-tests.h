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

  using RemoteModule = RemoteLoadModule<>;

  static zx::vmo GetTestVmo(std::string_view path);

  zx::vmo GetDepVmo(const RemoteModule::Soname& soname);

  template <class Diagnostics>
  void Load(Diagnostics& diag, std::string_view executable_name, bool should_fail) {
    const std::string executable_path = std::filesystem::path("test") / "bin" / executable_name;

    zx::vmo vmo;
    ASSERT_NO_FATAL_FAILURE(vmo = elfldltl::testing::GetTestLibVmo(executable_path));

    zx::vmo vdso_vmo;
    zx_status_t status = GetVdsoVmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vdso_vmo);
    ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);

    zx::vmo stub_ld_vmo;
    stub_ld_vmo = elfldltl::testing::GetTestLibVmo("ld-stub.so");

    // Pre-decode the vDSO and stub modules.
    constexpr size_t kVdso = 0, kStub = 1;
    auto predecode = [&diag](RemoteModule& module, std::string_view what, zx::vmo vmo) {
      // Set a temporary name until we decode the DT_SONAME.
      module.set_name(what);
      RemoteModule::size_type tls_id = 0;
      ASSERT_TRUE(module.Decode(diag, std::move(vmo), -1, tls_id));
      EXPECT_EQ(tls_id, 0u);
      EXPECT_THAT(module.decoded().needed(), ::testing::IsEmpty())
          << what << " cannot have DT_NEEDED";
      EXPECT_THAT(module.reloc_info().rel_relative(), ::testing::IsEmpty())
          << what << " cannot have RELATIVE relocations";
      EXPECT_THAT(module.reloc_info().rel_symbolic(), ::testing::IsEmpty())
          << what << " cannot have symbolic relocations";
      EXPECT_THAT(module.reloc_info().relr(), ::testing::IsEmpty())
          << what << " cannot have RELR relocations";
      std::visit(
          [what](const auto& jmprel) {
            EXPECT_THAT(jmprel, ::testing::IsEmpty())
                << what << " cannot have DT_JMPREL relocations";
          },
          module.reloc_info().jmprel());
      ASSERT_TRUE(module.HasModule());
      module.set_name(module.module().soname);
    };
    std::array<RemoteModule, 2> predecoded_modules;
    ASSERT_NO_FATAL_FAILURE(predecode(predecoded_modules[kVdso], "vDSO", std::move(vdso_vmo)));
    ASSERT_NO_FATAL_FAILURE(
        predecode(predecoded_modules[kStub], "stub ld.so", std::move(stub_ld_vmo)));

    // Acquire the layout details from the stub.  The same values collected
    // here can be reused along with the decoded RemoteLoadModule for the stub
    // for creating and populating the RemoteLoadModule for the passive ABI of
    // any number of separate dynamic linking domains in however many
    // processes.
    RemoteAbiStub<> abi_stub;
    EXPECT_TRUE(abi_stub.Init(diag, predecoded_modules[kStub]));
    EXPECT_GE(abi_stub.data_size(), sizeof(ld::abi::Abi<>) + sizeof(elfldltl::Elf<>::RDebug<>));
    EXPECT_LT(abi_stub.data_size(), zx_system_get_page_size());
    EXPECT_LE(abi_stub.abi_offset(), abi_stub.data_size() - sizeof(ld::abi::Abi<>));
    EXPECT_LE(abi_stub.rdebug_offset(), abi_stub.data_size() - sizeof(elfldltl::Elf<>::RDebug<>));
    EXPECT_NE(abi_stub.rdebug_offset(), abi_stub.abi_offset())
        << "with data_size() " << abi_stub.data_size();

    // Verify that the TLSDESC entry points were found in the stub and that their
    // addresses pass some basic smell tests.
    const RemoteModule& predecoded_stub = predecoded_modules[kStub];
    std::set<elfldltl::Elf<>::size_type> tlsdesc_entrypoints;
    const auto segment_is_executable = [](const auto& segment) -> bool {
      return segment.executable();
    };
    for (const elfldltl::Elf<>::size_type entry : abi_stub.tlsdesc_runtime()) {
      // Must be nonzero.
      EXPECT_NE(entry, 0u);

      // Must lie within the module bounds.
      EXPECT_GT(entry, predecoded_stub.load_info().vaddr_start());
      EXPECT_LT(entry - predecoded_stub.load_info().vaddr_start(),
                predecoded_stub.load_info().vaddr_size());

      // Must be inside an executable segment.
      auto segment = predecoded_stub.load_info().FindSegment(entry);
      ASSERT_NE(segment, predecoded_stub.load_info().segments().end());
      EXPECT_TRUE(std::visit(segment_is_executable, *segment));

      // Must be unique.
      auto [it, inserted] = tlsdesc_entrypoints.insert(entry);
      EXPECT_TRUE(inserted) << "duplicate entry point " << entry;
    }
    EXPECT_EQ(tlsdesc_entrypoints.size(), kTlsdescRuntimeCount);

    // First just decode all the modules: the executable and dependencies.
    auto get_dep_vmo = [this](const RemoteModule::Soname& soname) { return GetDepVmo(soname); };
    auto decode_result = RemoteModule::DecodeModules(diag, std::move(vmo), get_dep_vmo,
                                                     std::move(predecoded_modules));
    ASSERT_TRUE(decode_result);
    auto& modules = decode_result->modules;
    ASSERT_FALSE(modules.empty());

    RemoteModule& loaded_exec = modules.front();
    const RemoteModule::ExecInfo& exec_info = loaded_exec.decoded().exec_info();

    RemoteModule& loaded_stub = modules[decode_result->predecoded_positions[kStub]];
    RemoteModule& loaded_vdso = modules[decode_result->predecoded_positions[kStub]];

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
            << " second module loaded by " << loaded_by_name();
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

    // Record any stack size request from the executable's PT_GNU_STACK.
    set_stack_size(exec_info.stack_size);

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
    if (!RemoteModule::AllModulesDecoded(modules)) {
      // Whatever the failures were have already been diagnosed.
      // This isn't a test failure in LoadAndFail tests.
      EXPECT_EQ(this->HasFailure(), !should_fail);
      return;
    }

    // Now that the set of modules is known, initialize the remote ABI heap in
    // the loaded_stub module.  This can change that module's vaddr_size.
    RemoteAbi<> remote_abi;
    zx::result abi_result =
        remote_abi.Init(diag, abi_stub, loaded_stub, modules, decode_result->max_tls_modid);
    ASSERT_TRUE(abi_result.is_ok()) << abi_result.status_string();

    // Choose load addresses.
    EXPECT_TRUE(RemoteModule::AllocateModules(diag, modules, root_vmar().borrow()));

    // Acquire a StaticTlsDescResolver that uses the stub dynamic linker's
    // entry TLSDESC points.  Note this could in the general case be modified
    // later by:
    // `tls_desc_resolver.SetHook(TlsdescRuntime::kStatic, custom_hook);`
    auto tls_desc_resolver = abi_stub.tls_desc_resolver(loaded_stub.load_bias());

    // Apply relocations to segment VMOs.
    EXPECT_TRUE(RemoteModule::RelocateModules(diag, modules, tls_desc_resolver));

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

    // Now that load addresses have been chosen, populate the remote ABI data.
    abi_result = std::move(remote_abi).Finish(diag, abi_stub, loaded_stub, modules);
    EXPECT_TRUE(abi_result.is_ok()) << abi_result.status_string();

    // Finally, all the VMO contents are in place to be mapped into the
    // process.
    EXPECT_TRUE(RemoteModule::LoadModules(diag, modules));

    // Use the executable's entry point at its loaded address.
    set_entry(exec_info.relative_entry + loaded_exec.load_bias());

    // Locate the loaded vDSO to pass its base pointer to the test process.
    set_vdso_base(loaded_vdso.module().vaddr_start());

    RemoteModule::CommitModules(modules);
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
