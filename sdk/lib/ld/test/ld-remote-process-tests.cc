// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-remote-process-tests.h"

#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/ld/abi.h>
#include <lib/ld/remote-abi-stub.h>
#include <lib/ld/remote-load-module.h>
#include <lib/ld/testing/test-vmo.h>
#include <lib/zx/job.h>
#include <zircon/process.h>

#include <string_view>

namespace ld::testing {

constexpr std::string_view kLibprefix = LD_STARTUP_TEST_LIBPREFIX;
constexpr auto kLinkerName = ld::abi::Abi<>::kSoname;
class LdRemoteProcessTests::MockLoader {
 public:
  MOCK_METHOD(zx::vmo, LoadObject, (std::string));

  void ExpectLoadObject(std::string_view name) {
    const std::string path = std::filesystem::path("test") / "lib" / kLibprefix / name;
    EXPECT_CALL(*this, LoadObject(std::string{name}))
        .WillOnce(::testing::Return(elfldltl::testing::GetTestLibVmo(path)));
  }

 private:
  ::testing::InSequence sequence_guard_;
};

LdRemoteProcessTests::LdRemoteProcessTests() = default;

LdRemoteProcessTests::~LdRemoteProcessTests() = default;

void LdRemoteProcessTests::Init(std::initializer_list<std::string_view> args,
                                std::initializer_list<std::string_view> env) {
  mock_loader_ = std::make_unique<MockLoader>();

  std::string_view name = process_name();
  zx::process process;
  ASSERT_EQ(zx::process::create(*zx::job::default_job(), name.data(),
                                static_cast<uint32_t>(name.size()), 0, &process, &root_vmar_),
            ZX_OK);
  set_process(std::move(process));

  // Initialize a log to pass ExpectLog statements in load-tests.cc.
  fbl::unique_fd log_fd;
  ASSERT_NO_FATAL_FAILURE(InitLog(log_fd));

  ASSERT_EQ(zx::thread::create(this->process(), name.data(), static_cast<uint32_t>(name.size()), 0,
                               &thread_),
            ZX_OK);
}

// Set the expectations that these dependencies will be loaded in the given order.
void LdRemoteProcessTests::Needed(std::initializer_list<std::string_view> names) {
  for (std::string_view name : names) {
    // The linker and vdso should not be included in any `Needed` list for a
    // test, because load requests for them bypass the loader.
    ASSERT_TRUE(name != kLinkerName.str() && name != GetVdsoSoname().str())
        << std::string{name} + " should not be included in Needed list.";
    mock_loader_->ExpectLoadObject(name);
  }
}

void LdRemoteProcessTests::Load(std::string_view executable_name) {
  using RemoteModule = RemoteLoadModule<>;

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

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
    auto result = module.Decode(diag, std::move(vmo), -1);
    ASSERT_TRUE(result.is_ok());
    EXPECT_THAT(result->needed, ::testing::IsEmpty()) << what << " cannot have DT_NEEDED";
    EXPECT_THAT(module.reloc_info().rel_relative(), ::testing::IsEmpty())
        << what << " cannot have RELATIVE relocations";
    EXPECT_THAT(module.reloc_info().rel_symbolic(), ::testing::IsEmpty())
        << what << " cannot have symbolic relocations";
    EXPECT_THAT(module.reloc_info().relr(), ::testing::IsEmpty())
        << what << " cannot have RELR relocations";
    std::visit(
        [what](const auto& jmprel) {
          EXPECT_THAT(jmprel, ::testing::IsEmpty()) << what << " cannot have DT_JMPREL relocations";
        },
        module.reloc_info().jmprel());
    ASSERT_TRUE(module.HasModule());
    module.set_name(module.module().soname);
  };
  std::array<RemoteModule, 2> predecoded_modules;
  ASSERT_NO_FATAL_FAILURE(predecode(predecoded_modules[kVdso], "vDSO", std::move(vdso_vmo)));
  ASSERT_NO_FATAL_FAILURE(
      predecode(predecoded_modules[kStub], "stub ld.so", std::move(stub_ld_vmo)));

  // Acquire the layout details from the stub.  The same values collected here
  // can be reused along with the decoded RemoteLoadModule for the stub for
  // creating and populating the RemoteLoadModule for the passive ABI of any
  // number of separate dynamic linking domains in however many processes.
  RemoteAbiStub<> abi_stub;
  EXPECT_TRUE(abi_stub.Init(diag, predecoded_modules[kStub]));
  EXPECT_GE(abi_stub.data_size(), sizeof(ld::abi::Abi<>) + sizeof(elfldltl::Elf<>::RDebug<>));
  EXPECT_LT(abi_stub.data_size(), zx_system_get_page_size());
  EXPECT_LE(abi_stub.abi_offset(), abi_stub.data_size() - sizeof(ld::abi::Abi<>));
  EXPECT_LE(abi_stub.rdebug_offset(), abi_stub.data_size() - sizeof(elfldltl::Elf<>::RDebug<>));
  EXPECT_NE(abi_stub.rdebug_offset(), abi_stub.abi_offset())
      << "with data_size() " << abi_stub.data_size();

  auto get_dep_vmo = [this](const RemoteModule::Soname& soname) {
    return mock_loader_->LoadObject(std::string{soname.str()});
  };

  auto decode_result =
      RemoteModule::DecodeModules(diag, std::move(vmo), get_dep_vmo, std::move(predecoded_modules));
  ASSERT_TRUE(decode_result);
  set_stack_size(decode_result->main_exec.stack_size);

  auto& modules = decode_result->modules;
  ASSERT_FALSE(modules.empty());

  // TODO(https://fxbug.dev/318041873): Do the passive ABI layout in the stub
  // here.
  //RemoteModule& loaded_stub = modules[decode_result->predecoded_positions[kVdso]];

  EXPECT_TRUE(RemoteModule::AllocateModules(diag, modules, root_vmar().borrow()));
  EXPECT_TRUE(RemoteModule::RelocateModules(diag, modules));

  // TODO(https://fxbug.dev/318041873): Finish filling out passive ABI here.

  EXPECT_TRUE(RemoteModule::LoadModules(diag, modules));

  // The executable will always be the first module, retrieve it to set the
  // loaded entry point.
  set_entry(decode_result->main_exec.relative_entry + modules[kVdso].load_bias());

  // Locate the loaded vDSO to pass its base pointer to the test process.
  RemoteModule& loaded_vdso = modules[decode_result->predecoded_positions[kStub]];
  set_vdso_base(loaded_vdso.module().vaddr_start());

  RemoteModule::CommitModules(modules);
}

int64_t LdRemoteProcessTests::Run() {
  return LdLoadZirconProcessTestsBase::Run(nullptr, stack_size_, thread_, entry_, vdso_base_,
                                           root_vmar());
}

}  // namespace ld::testing
