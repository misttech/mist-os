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
#include <sys/mman.h>
#include <unistd.h>

#ifdef __Fuchsia__
#include <lib/zx/channel.h>
#include <zircon/syscalls.h>
#else
#include <sys/auxv.h>
#endif

#include <initializer_list>
#include <numeric>
#include <string_view>
#include <type_traits>

#include <gtest/gtest.h>

#include "posix.h"

namespace {

constexpr std::string_view kLdStartupName = LD_STARTUP_TEST_LIB;

// The in-process tests here work by doing ELF loading approximately as the
// system program loader would, but into this process that's running the test.
// Once the dynamic linker has been loaded, the InProcessTestLaunch object
// knows how its entry point wants to be called.  It's responsible for
// collecting the information to be passed to the dynamic linker, and then
// doing the call into its entry point to emulate what it would expect from the
// program loader starting an initial thread.
//
// The simple first version just takes a single argument string that the
// dynamic linker will receive.

#ifdef __Fuchsia__

// On Fuchsia this means packing a message on the bootstrap channel.  The entry
// point receives the bootstrap channel (zx_handle_t) and the base address of
// the vDSO.
class InProcessTestLaunch {
 public:
  // The object is default-constructed so Init() can be called inside
  // ASSERT_NO_FATAL_FAILURE(...).
  void Init(std::string_view str) {
    zx::channel write_bootstrap;
    ASSERT_EQ(zx::channel::create(0, &write_bootstrap, &read_bootstrap_), ZX_OK);
    ASSERT_EQ(write_bootstrap.write(0, str.data(), static_cast<uint32_t>(str.size()), nullptr, 0),
              ZX_OK);
  }

  int Call(uintptr_t entry) {
    auto fn = reinterpret_cast<EntryFunction*>(entry);
    return fn(read_bootstrap_.release(), GetVdso());
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

  // This is the receive end of the channel, transferred to the "new process".
  zx::channel read_bootstrap_;
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

// On POSIX-like systems this eventually will mean a canonical stack setup.
// For now, we're just passing the string pointer as is.
class InProcessTestLaunch {
 public:
  void Init(std::string_view str) {
    ASSERT_NO_FATAL_FAILURE(AllocateStack());
    ASSERT_NO_FATAL_FAILURE(PopulateStack({str}, {}));
  }

  int Call(uintptr_t entry) { return CallOnStack(entry, sp_); }

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

    // Compute the total number of pointers to write.
    size_t ptrs = argv.size() + 1 + envp.size() + 1;

    // The stack must fit all that plus the auxv block.
    ASSERT_LT(strings + 15 + (ptrs * sizeof(uintptr_t)) + sizeof(AuxvBlock), kStackSize);

    // Start at the top of the stack, and place the strings.
    std::byte* sp = static_cast<std::byte*>(stack_) + (kStackSize * 2);
    sp -= (strings + (ptrs * sizeof(uintptr_t)) + 15) & -size_t{16};
    cpp20::span string_space{reinterpret_cast<char*>(sp), strings};

    // Next, leave space for the auxv block, which can be filled in later.
    sp -= sizeof(AuxvBlock);
    AuxvBlock& auxv = *new (sp) AuxvBlock{};

    // Finally, the argc and pointers form what's seen right at the SP.
    sp -= (ptrs + 1) * sizeof(uintptr_t);
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

    // Fill in other auxv values specific to this test.
    // TODO(mcgrathr): this is where the loaded executable would be described
    auxv.entry.back() = 0;

    sp_ = sp;
  }

  void* stack_ = nullptr;
  void* sp_ = nullptr;
};

#endif  // __Fuchsia__

template <class LoaderTraits>
class LdStartupTests : public elfldltl::testing::LoadTests<LoaderTraits> {
 public:
  using Base = elfldltl::testing::LoadTests<LoaderTraits>;
  using typename Base::Loader;
  using typename Base::LoadResult;

  void SetUp() override { Load(); }

  void Load() {
    std::optional<LoadResult> result;
    ASSERT_NO_FATAL_FAILURE(Base::Load(kLdStartupName, result));
    loader_ = std::move(result->loader);
    entry_ = result->entry + loader_->load_bias();
  }

  uintptr_t entry() const { return entry_; }

 private:
  std::optional<Loader> loader_;
  uintptr_t entry_ = 0;
};

TYPED_TEST_SUITE(LdStartupTests, elfldltl::testing::LoaderTypes);

TYPED_TEST(LdStartupTests, Basic) {
  // The skeletal dynamic linker is hard-coded now to read its argument string
  // and return its length.
  constexpr std::string_view kArgument = "Lorem ipsum dolor sit amet";
  constexpr int kReturnValue = static_cast<int>(kArgument.size());

  InProcessTestLaunch launch;
  ASSERT_NO_FATAL_FAILURE(launch.Init(kArgument));

  EXPECT_EQ(launch.Call(this->entry()), kReturnValue);
}

}  // namespace
