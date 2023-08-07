// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dlfcn.h>
#include <lib/elfldltl/container.h>
#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/link.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/elfldltl/testing/loader.h>
#include <lib/ld/abi.h>
#include <lib/stdcompat/functional.h>
#include <unistd.h>

#ifdef __Fuchsia__
#include <lib/zx/channel.h>
#include <zircon/processargs.h>
#include <zircon/syscalls.h>
#else
#include <sys/auxv.h>
#include <sys/mman.h>
#endif

#include <filesystem>
#include <initializer_list>
#include <numeric>
#include <string>
#include <string_view>
#include <tuple>
#include <type_traits>

#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include <lib/ld/testing/test-log-socket.h>
#include <lib/ld/testing/test-processargs.h>
#else
#include "../posix.h"
#endif

namespace {

// The in-process tests here work by doing ELF loading approximately as the
// system program loader would, but into this process that's running the test.
// Once the dynamic linker has been loaded, the InProcessTestLaunch object
// knows how its entry point wants to be called.  It's responsible for
// collecting the information to be passed to the dynamic linker, and then
// doing the call into its entry point to emulate what it would expect from the
// program loader starting an initial thread.

#ifdef __Fuchsia__

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

  void Init(std::initializer_list<std::string_view> args = {}) {
    zx_vaddr_t test_base;
    ASSERT_EQ(zx::vmar::root_self()->allocate(
                  ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_EXECUTE, 0, kVmarSize,
                  &test_vmar_, &test_base),
              ZX_OK);

    ASSERT_NO_FATAL_FAILURE(log_.Init());
    procargs()  //
        .AddInProcessTestHandles()
        .AddDuplicateHandle(PA_VMAR_ROOT, test_vmar_.borrow())
        .AddFd(STDERR_FILENO, log_.TakeSocket())
        .SetArgs(args);
  }

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
    const std::string path = std::filesystem::path("test") / "bin" / name;
    zx::vmo vmo = elfldltl::testing::GetTestLibVmo(path);
    ASSERT_TRUE(vmo);
    ASSERT_NO_FATAL_FAILURE(procargs().AddHandle(PA_VMO_EXECUTABLE, std::move(vmo)));
  }

  ld::testing::TestProcessArgs& procargs() { return procargs_; }

  std::string CollectLog() { return std::move(log_).Read(); }

  int Call(uintptr_t entry) {
    auto fn = reinterpret_cast<EntryFunction*>(entry);
    zx::channel bootstrap = procargs_.PackBootstrap();
    return fn(bootstrap.release(), GetVdso());
  }

  ~InProcessTestLaunch() {
    if (test_vmar_) {
      EXPECT_EQ(test_vmar_.destroy(), ZX_OK);
    }
  }

 private:
  using EntryFunction = int(zx_handle_t, void*);

  static void* GetVdso() {
    static void* vdso = [] {
      Dl_info info;
      EXPECT_TRUE(dladdr(reinterpret_cast<void*>(&_zx_process_exit), &info));
      EXPECT_STREQ(info.dli_fname, "<vDSO>");
      return info.dli_fbase;
    }();
    return vdso;
  }

  ld::testing::TestProcessArgs procargs_;
  ld::testing::TestLogSocket log_;
  zx::vmar test_vmar_;
};

#else  // ! __Fuchsia__

// This is actually defined in assembly code with internal linkage.
// It simply switches to the new SP and then calls the entry point.
// When that code returns, this just restores the old SP and also returns.
extern "C" int CallOnStack(uintptr_t entry, void* sp);
__asm__(
    R"""(
    .pushsection .text.CallOnStack, "ax", %progbits
    .type CallOnstack, %function
    CallOnStack:
      .cfi_startproc
    )"""
#if defined(__aarch64__)
    R"""(
      stp x29, x30, [sp, #-16]!
      .cfi_adjust_cfa_offset 16
      mov x29, sp
      .cfi_def_cfa_register x29
      mov sp, x1
      blr x0
      mov sp, x29
      .cfi_def_cfa_register sp
      ldp x29, x30, [sp], #16
      .cfi_adjust_cfa_offset -16
      ret
    )"""
#elif defined(__x86_64__)
    // Note this stores our return address below the SP and then jumps, because
    // a call would move the SP.  The posix-startup.S entry point code expects
    // the StartupStack at the SP, not a return address.  Note this saves and
    // restores %rbx so that the entry point code can clobber it.
    // TODO(mcgrathr): For now, it then returns at the end, popping the stack.
    R"""(
      push %rbp
      .cfi_adjust_cfa_offset 8
      mov %rsp, %rbp
      .cfi_def_cfa_register %rbp
      .cfi_offset %rbp, -8*2
      push %rbx
      .cfi_offset %rbx, -8*3
      lea 0f(%rip), %rax
      mov %rsi, %rsp
      mov %rax, -8(%rsp)
      jmp *%rdi
    0:mov %rbp, %rsp
      .cfi_def_cfa_register %rsp
      mov -8(%rsp), %rbx
      .cfi_same_value %rbx
      pop %rbp
      .cfi_same_value %rbp
      .cfi_adjust_cfa_offset -8
      ret
    )"""
#else
#error "unsupported machine"
#endif
    R"""(
      .cfi_endproc
    .size CallOnStack, . - CallOnStack
    .popsection
    )""");

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

  void Init(std::initializer_list<std::string_view> args = {}) {
    ASSERT_NO_FATAL_FAILURE(AllocateStack());
    ASSERT_NO_FATAL_FAILURE(PopulateStack(args, {}));
  }

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
    auxv_->phnum.back() = phdrs.size();
    exec->info.VisitSegments([load_bias = exec->loader.load_bias(), offset = exec->phoff(),
                              filesz = phdrs.size_bytes(), this](const auto& segment) {
      if (segment.offset() <= offset && offset - segment.offset() < segment.filesz() &&
          segment.filesz() - (offset - segment.offset()) >= filesz) {
        auxv_->phdr.back() = offset - segment.offset() + segment.vaddr() + load_bias;
        return false;
      }
      return true;
    });
    ASSERT_NE(auxv_->phdr.back(), 0u);

    // Set AT_ENTRY to the executable's entry point.
    auxv_->entry.back() = exec->entry + exec->loader.load_bias();

    // Save the second Loader object to keep the mappings alive.
    exec_loader_ = std::move(exec->loader);
  }

  int Call(uintptr_t entry) { return CallOnStack(entry, sp_); }

  std::string CollectLog() { return {}; }

  ~InProcessTestLaunch() {
    if (stack_) {
      munmap(stack_, kStackSize * 2);
    }
  }

 private:
  struct AuxvBlock {
    ld::Auxv vdso = {
        static_cast<uintptr_t>(ld::AuxvTag::kSysinfoEhdr),
        getauxval(static_cast<uintptr_t>(ld::AuxvTag::kSysinfoEhdr)),
    };
    ld::Auxv pagesz = {
        static_cast<uintptr_t>(ld::AuxvTag::kPagesz),
        static_cast<uintptr_t>(sysconf(_SC_PAGE_SIZE)),
    };
    ld::Auxv phdr = {static_cast<uintptr_t>(ld::AuxvTag::kPhdr)};
    ld::Auxv phent = {
        static_cast<uintptr_t>(ld::AuxvTag::kPhent),
        sizeof(elfldltl::Elf<>::Phdr),
    };
    ld::Auxv phnum = {static_cast<uintptr_t>(ld::AuxvTag::kPhnum)};
    ld::Auxv entry = {static_cast<uintptr_t>(ld::AuxvTag::kEntry)};
    const ld::Auxv null = {static_cast<uintptr_t>(ld::AuxvTag::kNull)};
  };

  static constexpr size_t kStackSize = 64 << 10;

  void AllocateStack() {
    // Allocate a stack and a guard region below it.
    void* ptr =
        mmap(nullptr, kStackSize * 2, PROT_READ | PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
    ASSERT_TRUE(ptr) << "mmap: " << strerror(errno);
    stack_ = ptr;
    // Protect the guard region below the stack.
    EXPECT_EQ(mprotect(stack_, kStackSize, PROT_NONE), 0) << strerror(errno);
  }

  void PopulateStack(std::initializer_list<std::string_view> argv,
                     std::initializer_list<std::string_view> envp) {
    // Figure out the total size of string data to write.
    constexpr auto string_size = [](size_t total, std::string_view str) {
      return total + str.size() + 1;
    };
    const size_t strings =
        std::accumulate(argv.begin(), argv.end(),
                        std::accumulate(envp.begin(), envp.end(), 0, string_size), string_size);

    // Compute the total number of pointers to write (after the argc word).
    size_t ptrs = argv.size() + 1 + envp.size() + 1;

    // The stack must fit all that plus the auxv block.
    ASSERT_LT(strings + 15 + ((1 + ptrs) * sizeof(uintptr_t)) + sizeof(AuxvBlock), kStackSize);

    // Start at the top of the stack, and place the strings.
    std::byte* sp = static_cast<std::byte*>(stack_) + (kStackSize * 2);
    sp -= strings;
    cpp20::span string_space{reinterpret_cast<char*>(sp), strings};

    // Adjust down so everything will be aligned.
    const size_t strings_and_ptrs = strings + ((1 + ptrs) * sizeof(uintptr_t));
    sp -= ((strings_and_ptrs + 15) & -size_t{16}) - strings_and_ptrs;

    // Next, leave space for the auxv block, which can be filled in later.
    static_assert(sizeof(AuxvBlock) % 16 == 0);
    sp -= sizeof(AuxvBlock);
    auxv_ = new (sp) AuxvBlock;

    // Finally, the argc and pointers form what's seen right at the SP.
    sp -= (1 + ptrs) * sizeof(uintptr_t);
    ld::StartupStack* startup = new (sp) ld::StartupStack{.argc = argv.size()};
    cpp20::span string_ptrs{startup->argv, ptrs};

    // Now copy the strings and write the pointers to them.
    for (auto list : {argv, envp}) {
      for (std::string_view str : list) {
        string_ptrs.front() = string_space.data();
        string_ptrs = string_ptrs.subspan(1);
        string_space = string_space.subspan(str.copy(string_space.data(), string_space.size()));
        string_space.front() = '\0';
        string_space = string_space.subspan(1);
      }
      string_ptrs.front() = nullptr;
      string_ptrs = string_ptrs.subspan(1);
    }
    ASSERT_TRUE(string_ptrs.empty());
    ASSERT_TRUE(string_space.empty());

    ASSERT_EQ(reinterpret_cast<uintptr_t>(sp) % 16, 0u);
    sp_ = sp;
  }

  elfldltl::MmapLoader loader_, exec_loader_;
  void* stack_ = nullptr;
  void* sp_ = nullptr;
  AuxvBlock* auxv_ = nullptr;
};

#endif  // __Fuchsia__

template <class LoaderTraits>
class LdLoadTests : public elfldltl::testing::LoadTests<LoaderTraits> {
 public:
  using Base = elfldltl::testing::LoadTests<LoaderTraits>;
  using typename Base::Loader;
  using typename Base::LoadResult;

  template <class Launch>
  void LaunchExecutable(std::string_view executable_name, Launch&& launch) {
    std::optional<LoadResult> result;
    ASSERT_NO_FATAL_FAILURE(std::apply(
        [this, &result](auto&&... args) {
          this->Load(kLdStartupName, result,
                     // LoaderArgs() gives args for LoaderTraits::MakeLoader().
                     std::forward<decltype(args)>(args)...);
        },
        launch.LoaderArgs()));
    entry_ = result->entry + result->loader.load_bias();
    ASSERT_NO_FATAL_FAILURE(launch.AfterLoad(std::move(result->loader)));
    ASSERT_NO_FATAL_FAILURE(launch.SendExecutable(executable_name, *this));
  }

  template <class Launch>
  int Invoke(Launch&& launch) {
    return std::forward<Launch>(launch).Call(entry_);
  }

 private:
#ifdef __Fuchsia__
  static constexpr std::string_view kLibprefix = LD_STARTUP_TEST_LIBPREFIX;
  inline static const std::string kLdStartupName =
      std::string("test/lib/") + std::string(kLibprefix) + std::string(ld::abi::kInterp);
#else
  static constexpr std::string_view kLdStartupName = ld::abi::kInterp;
#endif

  uintptr_t entry_ = 0;
};

#ifdef __Fuchsia__
// Don't test MmapLoaderTraits on Fuchsia since it can't clean up after itself.
using LoaderTypes = ::testing::Types<elfldltl::testing::LocalVmarLoaderTraits,
                                     elfldltl::testing::RemoteVmarLoaderTraits>;
#else
using LoaderTypes = elfldltl::testing::LoaderTypes;
#endif

TYPED_TEST_SUITE(LdLoadTests, LoaderTypes);

TYPED_TEST(LdLoadTests, Basic) {
  constexpr int kReturnValue = 17;

  InProcessTestLaunch launch;
  ASSERT_NO_FATAL_FAILURE(launch.Init());

  ASSERT_NO_FATAL_FAILURE(this->LaunchExecutable("ret17", launch));

  EXPECT_EQ(this->Invoke(launch), kReturnValue);

  if constexpr (InProcessTestLaunch::kHasLog) {
    EXPECT_EQ(launch.CollectLog(), "");
  }
}

}  // namespace
