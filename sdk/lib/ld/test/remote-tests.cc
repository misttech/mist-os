// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/ld/remote-abi-stub.h>
#include <lib/ld/remote-dynamic-linker.h>

#include <gtest/gtest.h>

#include "ld-remote-process-tests.h"

namespace {

// These tests reuse the fixture that supports the LdLoadTests (load-tests.cc)
// for the common handling of creating and launching a Zircon process.  The
// Load method is not used here, since that itself uses the RemoteDynamicLinker
// API under the covers, and the tests here are for that API surface itself.
using LdRemoteTests = ld::testing::LdRemoteProcessTests;

// This is the basic examplar of using the API to load a main executable in the
// standard way.
TEST_F(LdRemoteTests, RemoteDynamicLinker) {
  constexpr int64_t kReturnValue = 17;

  // The Init() method in the test fixture handles creating a process and such.
  // This is outside the scope of the ld::RemoteDynamicLinker API.
  ASSERT_NO_FATAL_FAILURE(Init());

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  // Acquire the layout details from the stub.  The same ld::RemoteAbiStub
  // object can be reused for creating and populating the passive ABI of any
  // number of separate dynamic linking domains in however many processes.
  //
  // The TakeSubLdVmo() method in the test fixture returns the (read-only,
  // executable) zx::vmo for the stub dynamic linker provided along with the
  // //sdk/lib/ld library and packaged somewhere with the code using this API.
  // The user of the API must acquire such a VMO by their own means.
  Linker linker;
  linker.set_abi_stub(ld::RemoteAbiStub<>::Create(diag, TakeStubLdVmo(), kPageSize));
  ASSERT_TRUE(linker.abi_stub());

  // The main executable is an ELF file in a VMO.  The GetExecutableVmo()
  // method in the test fixture returns the (read-only, executable) zx::vmo for
  // the main executable.  The user of the API must acquire this VMO by their
  // own means.
  zx::vmo exec_vmo;
  ASSERT_NO_FATAL_FAILURE(exec_vmo = GetExecutableVmo("many-deps"));

  // Decode the main executable.  This transfers ownership of the zx::vmo for
  // the executable into the new fbl::RefPtr<ld::RemoteDecodedModule> object.
  // If there were decoding problems they will have been reported to the
  // Diagnostics template API object.  If that object said to bail out after an
  // error or warning, Create returns a null RefPtr.  If it said to keep going
  // after an error, then an object was created but may be incomplete: it can
  // be used in ld::RemoteDynamicLinker::Init, but may not be in a fit state to
  // attempt relocation.
  Linker::Module::DecodedPtr decoded_executable =
      Linker::Module::Decoded::Create(diag, std::move(exec_vmo), kPageSize);
  EXPECT_TRUE(decoded_executable);

  // If the program is meant to make Zircon system calls, then it needs a vDSO,
  // in the form of a (read-only, executable) zx::vmo handle to one of the
  // kernel's blessed vDSO VMOs.  The GetVdsoVmo() function in the testing
  // library returns the same one used by the test itself.  The user of the API
  // must acquire the desired vDSO VMO by their own means.
  zx::vmo vdso_vmo;
  zx_status_t status = ld::testing::GetVdsoVmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vdso_vmo);
  EXPECT_EQ(status, ZX_OK) << zx_status_get_string(status);

  // Decode the vDSO, just as done for the main executable.  The DecodedPtr
  // references can be cached and reused for any VMO of an ELF file.
  Linker::Module::DecodedPtr decoded_vdso =
      Linker::Module::Decoded::Create(diag, std::move(vdso_vmo), kPageSize);
  EXPECT_TRUE(decoded_vdso);

  // The get_dep callback is any object callable as GetDepResult(Soname).  It
  // returns std::nullopt for missing dependencies, or a DecodedPtr.  The
  // GetDepFunction() in the test fixture returns an object that approximates
  // for the test context something like looking up files in /pkg/lib as is
  // done via fuchsia.ldsvc FIDL protocols by the usual in-process dynamic
  // linker.  The Needed() method in the test fixture indicates the expected
  // sequence of requests and collects those files from the test package's
  // special directory layout.  The user of the API must supply a callback that
  // turns strings into appropriate ld::RemoteDecodedModule::Ptr refs.  The
  // callback returns std::nullopt to bail out after a failure; the
  // RemoteDynamicLinker does not do any logging about this directly, so the
  // callback itself should do so.  The callback may also return a null Ptr
  // instead to indicate work should keep going despite the missing file.  This
  // will likely result in more errors later, such as undefined symbols; but it
  // gives the opportunity to report more missing files before bailing out.
  auto get_dep = GetDepFunction(diag);
  ASSERT_NO_FATAL_FAILURE(Needed({
      "libld-dep-a.so",
      "libld-dep-b.so",
      "libld-dep-f.so",
      "libld-dep-c.so",
      "libld-dep-d.so",
      "libld-dep-e.so",
  }));

  // Init() decodes everything and loads all the dependencies.
  auto init_result = linker.Init(
      // Any <lib/elfldltl/diagnostics.h> template API object can be used.
      diag,
      // The InitModuleList argument is a std::vector, so it can be constructed
      // in many ways including an initializer list.  For inividual InitModule
      // elements there is a convenient factory function that suits each use
      // case.  The order of the root modules is important: it becomes the
      // "load order" used for symbol resolution and seen in the passive
      // ABI--but usually that's just the main executable.  Implicit modules
      // can appear in any order with respect to each other or the root
      // modules; the only effect is on the relative order of any unreferenced
      // implicit modules at the end of the ld::RemoteDynamicLinker::modules()
      // "load order" list.
      {Linker::Executable(std::move(decoded_executable)),
       Linker::Implicit(std::move(decoded_vdso))},
      get_dep);
  ASSERT_TRUE(init_result);

  // The return value is a vector parallel to the InitModuleList passed in.
  ASSERT_EQ(init_result->size(), 2u);

  // Allocate() chooses load addresses by creating new child VMARs within some
  // given parent VMAR, such as the root VMAR of a new process.
  EXPECT_TRUE(linker.Allocate(diag, root_vmar().borrow()));

  // The corresponding return vector element is an iterator into the
  // ld::RemoteDynamicLinker::modules() list.  After Allocate, the vaddr
  // details of each module have been decided.  The vDSO base address is
  // usually passed as the main executable entry point's second argument when
  // the process is launched via zx::process::start.  The test fixture's Run()
  // method passes this to zx::process::start, but launching the process is
  // outside the scope of this API.
  const Linker::Module& loaded_vdso = *init_result->back();
  set_vdso_base(loaded_vdso.module().vaddr_start());

  // main_entry() yields the runtime entry point address of the main (first)
  // root module, usually the main executable.  Naturally, it's only valid
  // after a successful Allocate phase.  The test fixture's Run() method passes
  // this to zx::process::start, but launching the process is outside the scope
  // of this API.
  set_entry(linker.main_entry());

  // main_stack_size() yields either std::nullopt or a specific stack size
  // requested by the executable's PT_GNU_STACK program header.  The test
  // fixture's Run() method uses this to allocate a stack and pass the initial
  // SP in zx::process::start; stack setup is outside the scope of this API.
  set_stack_size(linker.main_stack_size());

  // Relocate() applies relocations to segment VMOs.  This is the last place
  // that anything can usually go wrong due to a missing or invalid ELF file,
  // undefined symbol, or such problems with dynamic linking per se.
  EXPECT_TRUE(linker.Relocate(diag));

  // Finally, all the VMO contents are in place to be mapped into the process.
  // If this fails, it will be because of some system problem like resource
  // exhaustion rather than something about dynamic linking.
  ASSERT_TRUE(linker.Load(diag));

  // Any failure before here would destroy all the VMARs when the linker object
  // goes out of scope.  From here the mappings will stick in the process.
  linker.Commit();

  // The test fixture method does the rest of the work of launching the
  // process, all of which is out of the scope of this API:
  //  1. stack setup
  //  2. preparing a channel for the process bootstrap protocol
  //  3. calling zx::process::start with initial PC (e.g. from main_entry()),
  //     SP (from the stack setup), and the two entry point arguments:
  //      * some Zircon handle, usually the channel from which the process
  //        expects to read the message(s) of the process bootstrap protocol;
  //      * some integer, usually the base address where the vDSO was loaded,
  //        e.g. from `.module().vaddr_start` on the Linker::Module object for
  //        the vDSO, an implicit module found via Init()'s return value.
  // The test fixture method yields the process exit status when it finishes.
  EXPECT_EQ(Run(), kReturnValue);

  // The test fixture collected any output from the process and requires that
  // it be checked.
  ExpectLog("");
}

// This demonstrates using ld::RemoteDynamicLinker::Preplaced in the initial
// modules list.
TEST_F(LdRemoteTests, Preplaced) {
  constexpr uint64_t kLoadAddress = 0x12340000;

  ASSERT_NO_FATAL_FAILURE(Init());

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  Linker linker;
  linker.set_abi_stub(ld::RemoteAbiStub<>::Create(diag, TakeStubLdVmo(), kPageSize));
  ASSERT_TRUE(linker.abi_stub());

  zx::vmo exec_vmo;
  ASSERT_NO_FATAL_FAILURE(exec_vmo = GetExecutableVmo("fixed-load-address"));

  Linker::Module::DecodedPtr decoded_executable =
      Linker::Module::Decoded::Create(diag, std::move(exec_vmo), kPageSize);
  EXPECT_TRUE(decoded_executable);

  zx::vmo vdso_vmo;
  zx_status_t status = ld::testing::GetVdsoVmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vdso_vmo);
  EXPECT_EQ(status, ZX_OK) << zx_status_get_string(status);

  Linker::Module::DecodedPtr decoded_vdso =
      Linker::Module::Decoded::Create(diag, std::move(vdso_vmo), kPageSize);
  EXPECT_TRUE(decoded_vdso);

  auto init_result = linker.Init(  //
      diag,
      {Linker::Preplaced(std::move(decoded_executable), kLoadAddress,
                         ld::abi::Abi<>::kExecutableName),
       Linker::Implicit(std::move(decoded_vdso))},
      GetDepFunction(diag));
  ASSERT_TRUE(init_result);

  EXPECT_TRUE(linker.Allocate(diag, root_vmar().borrow()));
  set_entry(linker.main_entry());
  set_stack_size(linker.main_stack_size());
  set_vdso_base(init_result->back()->module().vaddr_start());

  EXPECT_EQ(init_result->front()->module().vaddr_start, kLoadAddress);

  EXPECT_TRUE(linker.Relocate(diag));
  ASSERT_TRUE(linker.Load(diag));
  linker.Commit();

  EXPECT_EQ(Run(), static_cast<int64_t>(kLoadAddress));

  ExpectLog("");
}

// This demonstrates performing two separate dynamic linking sessions to
// establish two distinct dynamic linking namespaces inside one process address
// space, where the second session uses the first session's initial modules
// (but not their dependencies) as preloaded implicit modules that can satisfy
// its symbols.
TEST_F(LdRemoteTests, SecondSession) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(Init());

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  // The ld::RemoteAbiStub only needs to be set up once for all sessions.
  ld::RemoteAbiStub<>::Ptr abi_stub = ld::RemoteAbiStub<>::Create(diag, TakeStubLdVmo(), kPageSize);
  ASSERT_TRUE(abi_stub);

  // First do a complete dynamic linking session for the main executable.
  ASSERT_NO_FATAL_FAILURE(Needed({
      "libindirect-deps-a.so",
      "libindirect-deps-b.so",
      "libindirect-deps-c.so",
  }));
  constexpr std::string_view kMainSoname = "libsecond-session-test.so.1";
  Linker::InitModuleList initial_modules;
  std::string vdso_soname;
  {
    Linker linker;
    linker.set_abi_stub(abi_stub);

    zx::vmo exec_vmo;
    ASSERT_NO_FATAL_FAILURE(exec_vmo = GetExecutableVmo("second-session"));

    Linker::Module::DecodedPtr decoded_executable =
        Linker::Module::Decoded::Create(diag, std::move(exec_vmo), kPageSize);
    EXPECT_TRUE(decoded_executable);

    zx::vmo vdso_vmo;
    zx_status_t status = ld::testing::GetVdsoVmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vdso_vmo);
    EXPECT_EQ(status, ZX_OK) << zx_status_get_string(status);

    Linker::Module::DecodedPtr decoded_vdso =
        Linker::Module::Decoded::Create(diag, std::move(vdso_vmo), kPageSize);
    EXPECT_TRUE(decoded_vdso);
    vdso_soname = decoded_vdso->soname().str();

    auto init_result = linker.Init(  //
        diag,
        {Linker::Executable(std::move(decoded_executable)),
         Linker::Implicit(std::move(decoded_vdso))},
        GetDepFunction(diag));
    ASSERT_TRUE(init_result);

    // Check on expected get_dep callbacks made by Init,
    // and wipe the test fixture mock clean for the later session.
    ASSERT_NO_FATAL_FAILURE(VerifyAndClearNeeded());

    EXPECT_TRUE(linker.Allocate(diag, root_vmar().borrow()));
    set_entry(linker.main_entry());
    set_stack_size(linker.main_stack_size());
    set_vdso_base(init_result->back()->module().vaddr_start());

    EXPECT_TRUE(linker.Relocate(diag));
    ASSERT_TRUE(linker.Load(diag));
    linker.Commit();

    // Extract both initial modules to be preloaded implicit modules.
    initial_modules = linker.PreloadedImplicit(*init_result);
    EXPECT_EQ(initial_modules.size(), 2u);

    // The primary domain has more modules than just those.
    EXPECT_GT(linker.modules().size(), 2u);
  }

  // Start the process running now with just the primary domain in place.
  // It will block on reading from the bootstrap channel.
  zx::channel bootstrap_sender, bootstrap_receiver;
  zx_status_t status = zx::channel::create(0, &bootstrap_sender, &bootstrap_receiver);
  ASSERT_EQ(status, ZX_OK) << "zx_channel_create: " << zx_status_get_string(status);
  ASSERT_NO_FATAL_FAILURE(Start(std::move(bootstrap_receiver)));

  // Now do a second session using the InitModule::AlreadyLoaded main
  // executable and vDSO from the first session as implicit modules.
  Linker::size_type test_start_fnptr = 0;
  {
    Linker second_linker;
    second_linker.set_abi_stub(abi_stub);

    // Acquire the VMO for the root module.
    constexpr Linker::Soname kRootModule{"second-session-module.so"};
    zx::vmo module_vmo;
    ASSERT_NO_FATAL_FAILURE(
        module_vmo = ld::testing::MockLoaderServiceForTest::GetRootModuleVmo(kRootModule.str()));

    // Decode the root module.
    Linker::Module::DecodedPtr decoded_module =
        Linker::Module::Decoded::Create(diag, std::move(module_vmo), kPageSize);
    EXPECT_TRUE(decoded_module);

    // Add in the root module with the implicit modules from the first session.
    initial_modules.emplace_back(Linker::RootModule(decoded_module, kRootModule));

    // Prime fresh expectations for get_dep callbacks from this session.
    ASSERT_NO_FATAL_FAILURE(VerifyAndClearNeeded());
    constexpr std::string_view kDepModule = "libsecond-session-module-deps-a.so";
    ASSERT_NO_FATAL_FAILURE(Needed({kDepModule}));

    // Now resolve dependencies, including the preloaded implicit modules as
    // well as that Needed list, modules newly opened via the get_dep callback.
    auto init_result = second_linker.Init(diag, std::move(initial_modules), GetDepFunction(diag));
    ASSERT_TRUE(init_result);
    ASSERT_EQ(init_result->size(), 3u);

    EXPECT_EQ(init_result->front()->name().str(), kMainSoname);
    EXPECT_EQ(init_result->at(1)->name().str(), vdso_soname);
    EXPECT_EQ(init_result->back()->name().str(), kRootModule.str());

    EXPECT_TRUE(init_result->front()->preloaded());
    EXPECT_TRUE(init_result->at(1)->preloaded());
    EXPECT_FALSE(init_result->back()->preloaded());

    ASSERT_EQ(second_linker.modules().size(), 5u);
    EXPECT_EQ(second_linker.modules().at(0).name().str(), kRootModule.str());
    EXPECT_EQ(second_linker.modules().at(1).name().str(), kMainSoname);
    EXPECT_TRUE(second_linker.modules().at(1).preloaded());
    EXPECT_EQ(second_linker.modules().at(2).name().str(), kDepModule);
    EXPECT_EQ(second_linker.modules().at(3).name().str(), vdso_soname);
    EXPECT_TRUE(second_linker.modules().at(3).preloaded());
    EXPECT_EQ(second_linker.modules().at(4).name().str(), ld::abi::Abi<>::kSoname.str());

    ASSERT_NO_FATAL_FAILURE(VerifyAndClearNeeded());

    // Allocate should place the root module and leave preloaded ones alone.
    EXPECT_TRUE(second_linker.Allocate(diag, root_vmar().borrow()));
    EXPECT_TRUE(init_result->front()->preloaded());
    EXPECT_TRUE(init_result->at(1)->preloaded());
    EXPECT_FALSE(init_result->back()->preloaded());

    // Finish dynamic linking.
    EXPECT_TRUE(second_linker.Relocate(diag));
    ASSERT_TRUE(second_linker.Load(diag));
    second_linker.Commit();

    // Look up the module's entry-point symbol.
    constexpr elfldltl::SymbolName kTestStart{"TestStart"};
    auto* symbol = kTestStart.Lookup(second_linker.main_module().module().symbols);
    ASSERT_TRUE(symbol);
    test_start_fnptr = symbol->value + second_linker.main_module().load_bias();
  }
  EXPECT_NE(test_start_fnptr, 0u);

  // The process is already running and it will block until it reads the
  // function pointer from the bootstrap channel.
  status = bootstrap_sender.write(0, &test_start_fnptr, sizeof(test_start_fnptr), nullptr, 0);
  ASSERT_EQ(status, ZX_OK) << "zx_channel_write: " << zx_status_get_string(status);

  // Close our end of the channel before waiting for the process, just in case
  // that kicks it out of a block and into crashing rather than wedging.
  bootstrap_sender.reset();

  // The process should now call TestStart() and exit with its return value.
  EXPECT_EQ(Wait(), kReturnValue);

  ExpectLog("");
}

TEST_F(LdRemoteTests, RemoteAbiStub) {
  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  // Acquire the layout details from the stub.  The same values collected here
  // can be reused along with the decoded RemoteLoadModule for the stub for
  // creating and populating the RemoteLoadModule for the passive ABI of any
  // number of separate dynamic linking domains in however many processes.
  ld::RemoteAbiStub<>::Ptr abi_stub = ld::RemoteAbiStub<>::Create(diag, TakeStubLdVmo(), kPageSize);
  ASSERT_TRUE(abi_stub);
  EXPECT_GE(abi_stub->data_size(), sizeof(ld::abi::Abi<>) + sizeof(elfldltl::Elf<>::RDebug<>));
  EXPECT_LT(abi_stub->data_size(), kPageSize);
  EXPECT_LE(abi_stub->abi_offset(), abi_stub->data_size() - sizeof(ld::abi::Abi<>));
  EXPECT_LE(abi_stub->rdebug_offset(), abi_stub->data_size() - sizeof(elfldltl::Elf<>::RDebug<>));
  EXPECT_NE(abi_stub->rdebug_offset(), abi_stub->abi_offset())
      << "with data_size() " << abi_stub->data_size();

  // Verify that the TLSDESC entry points were found in the stub and that
  // their addresses pass some basic smell tests.
  std::set<elfldltl::Elf<>::size_type> tlsdesc_entrypoints;
  const auto segment_is_executable = [](const auto& segment) -> bool {
    return segment.executable();
  };
  const Linker::Module::Decoded& stub_module = *abi_stub->decoded_module();
  for (const elfldltl::Elf<>::size_type entry : abi_stub->tlsdesc_runtime()) {
    // Must be nonzero.
    EXPECT_NE(entry, 0u);

    // Must lie within the module bounds.
    EXPECT_GT(entry, stub_module.load_info().vaddr_start());
    EXPECT_LT(entry - stub_module.load_info().vaddr_start(), stub_module.load_info().vaddr_size());

    // Must be inside an executable segment.
    auto segment = stub_module.load_info().FindSegment(entry);
    ASSERT_NE(segment, stub_module.load_info().segments().end());
    EXPECT_TRUE(std::visit(segment_is_executable, *segment));

    // Must be unique.
    auto [it, inserted] = tlsdesc_entrypoints.insert(entry);
    EXPECT_TRUE(inserted) << "duplicate entry point " << entry;
  }
  EXPECT_EQ(tlsdesc_entrypoints.size(), ld::kTlsdescRuntimeCount);
}

TEST_F(LdRemoteTests, LoadedBy) {
  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  // Acquire the layout details from the stub.  The same values collected here
  // can be reused along with the decoded RemoteLoadModule for the stub for
  // creating and populating the RemoteLoadModule for the passive ABI of any
  // number of separate dynamic linking domains in however many processes.
  Linker linker;
  linker.set_abi_stub(ld::RemoteAbiStub<>::Create(diag, TakeStubLdVmo(), kPageSize));
  ASSERT_TRUE(linker.abi_stub());

  // Decode the main executable.
  zx::vmo vmo;
  ASSERT_NO_FATAL_FAILURE(vmo = GetExecutableVmo("many-deps"));

  // Prime expectations for its dependencies.
  ASSERT_NO_FATAL_FAILURE(Needed({
      "libld-dep-a.so",
      "libld-dep-b.so",
      "libld-dep-f.so",
      "libld-dep-c.so",
      "libld-dep-d.so",
      "libld-dep-e.so",
  }));

  Linker::InitModuleList initial_modules{{
      Linker::Executable(Linker::Module::Decoded::Create(diag, std::move(vmo), kPageSize)),
  }};
  ASSERT_TRUE(initial_modules.front().decoded_module);
  ASSERT_TRUE(initial_modules.front().decoded_module->HasModule());

  // Pre-decode the vDSO.
  zx::vmo vdso_vmo;
  zx_status_t status = ld::testing::GetVdsoVmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vdso_vmo);
  ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);

  initial_modules.push_back(
      Linker::Implicit(Linker::Module::Decoded::Create(diag, std::move(vdso_vmo), kPageSize)));
  ASSERT_TRUE(initial_modules.back().decoded_module);
  ASSERT_TRUE(initial_modules.back().decoded_module->HasModule());

  auto init_result = linker.Init(diag, initial_modules, GetDepFunction(diag));
  ASSERT_TRUE(init_result);
  ASSERT_EQ(init_result->size(), initial_modules.size());

  // The root module went on the list first.
  const auto& modules = linker.modules();
  EXPECT_EQ(init_result->front(), modules.begin());

  // The vDSO module went somewhere on the list.
  EXPECT_NE(init_result->back(), modules.end());

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
            << "visible module " << next_module->name().str() << " loaded by " << loaded_by_name();
      } else {
        // A predecoded module was not referenced, so it's loaded by no-one.
        EXPECT_EQ(next_module->loaded_by_modid(), std::nullopt)
            << "invisible module " << next_module->name().str() << " loaded by "
            << loaded_by_name();
      }
    }
  }
}

}  // namespace
