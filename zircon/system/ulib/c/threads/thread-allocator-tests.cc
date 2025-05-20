// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/machine.h>
#include <lib/fit/function.h>
#include <lib/ld/testing/startup-ld-abi.h>
#include <lib/ld/tls.h>
#include <lib/zx/object.h>
#include <lib/zx/process.h>
#include <lib/zx/vmar.h>

#include <array>
#include <cstddef>
#include <map>
#include <ranges>
#include <thread>

#include <zxtest/zxtest.h>

#include "../test/safe-zero-construction.h"
#include "thread-allocator.h"
#include "threads_impl.h"
#include "tls-dep.h"

namespace LIBC_NAMESPACE_DECL {
namespace {

using TlsLayout = elfldltl::TlsLayout<>;
using TlsTraits = elfldltl::TlsTraits<>;

using InitializeTlsFn = void(std::span<std::byte> thread_block, ptrdiff_t tp_offset);

class LibcThreadTests : public ::zxtest::Test {
 public:
  // Place everything inside a constrained VMAR that's always destroyed at the
  // end of the test, just in case.
  static inline const PageRoundedSize kTestVmarSize{1 << 30};

  // Tests can set these so ThreadAllocator::Allocate will use them.
  // They're reset for each test.
  static inline TlsLayout gTlsLayout;
  static inline fit::function<InitializeTlsFn> gInitializeTls;

  void SetUp() override {
    gTlsLayout = {};
    gInitializeTls = {};
  }

  // Get the test VMAR, setting it up if need be.
  zx::unowned_vmar TestVmar(PageRoundedSize size = kTestVmarSize) {
    if (size != test_vmar_size_) {
      if (test_vmar_) {
        EXPECT_OK(test_vmar_.destroy());
        test_vmar_.reset();
      }
      uintptr_t test_vmar_base;
      EXPECT_OK(zx::vmar::root_self()->allocate(  //
          kTestVmarOptions, 0, size.get(), &test_vmar_, &test_vmar_base));
      test_vmar_size_ = size;
    }
    return test_vmar_.borrow();
  }

  void TearDown() override {
    if (test_vmar_) {
      ASSERT_OK(test_vmar_.destroy());
    }
  }

 private:
  static constexpr zx_vm_option_t kTestVmarOptions = ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE;

  zx::vmar test_vmar_;
  PageRoundedSize test_vmar_size_;
};

constexpr std::string_view kThreadName = "thread-allocator-test";

const PageRoundedSize kOnePage{1};
const PageRoundedSize kManyPages = kOnePage * 256;

constexpr size_t kStackCount = 2 + (kShadowCallStackAbi ? 1 : 0);

// This prevents the compiler from thinking it knows what the returned pointer
// is, so it must really do a load to read *ptr if that value is used; and must
// assume *ptr contains global state meaningful elsewhere and so actually do a
// store for `*ptr = ...`.
auto* Launder(auto* ptr) {
  __asm__ volatile("" : "=r"(ptr) : "0"(ptr));
  return ptr;
}

// Returns a death lambda for trying to read *ptr.
auto DeathByRead(const auto* ptr) {
  // The empty asm prevents the compiler from thinking it can elide the load
  // because it doesn't need the value, while Launder prevents it from finding
  // the value somewhere other than by doing that load.
  return [ptr] { __asm__ volatile("" : : "r"(*Launder(ptr))); };
}

// Returns a death lambda for trying to write *ptr.
auto DeathByWrite(auto* ptr, std::decay_t<decltype(*ptr)> value = {}) {
  return [ptr, value] { *Launder(ptr) = value; };
}

// There is never a `new Thread` actually done.  The memory is just zero-filled
// by the system, and then individual fields get set.  Ensure nobody can tell.
TEST_F(LibcThreadTests, SafeZeroConstruction) {
  LIBC_NAMESPACE::ExpectSafeZeroConstruction<Thread>();
}

TEST_F(LibcThreadTests, TpSelfPointer) {
  if constexpr (!TlsTraits::kTpSelfPointer) {
    ZXTEST_SKIP() << "no $tp -> self pointer in this machine's ABI";
    return;
  }

  // This is just testing the running system, not any code "under test".
  // But it verifies the TlsTraits expectation about the machine's ABI.
  const void* const* tp = ld::TpRelative<const void*>(0);
  EXPECT_EQ(tp, *tp);
}

// This expects everything except the Thread::abi slots to be all zero bytes.
void CheckZeroThread(const Thread* thread) {
  static constexpr std::array<std::byte, sizeof(Thread)> kZero{};

  Thread tcb;
  memcpy(&tcb, thread, sizeof(tcb));
  tcb.abi = {};
  if constexpr (TlsTraits::kTpSelfPointer) {
    tcb.head.tp = 0;
  }

  EXPECT_BYTES_EQ(&tcb, kZero.data(), kZero.size());
}

void CheckThread(Thread* thread) {
  // The Thread pointer is available.
  ASSERT_NE(thread, nullptr);
  CheckZeroThread(thread);

  if constexpr (TlsTraits::kTpSelfPointer) {
    // *$tp = $tp is already set.
    void* tp = pthread_to_tp(thread);
    EXPECT_EQ(tp, *Launder(ld::TpRelative<void*>(0, tp)))
        << "Thread @ " << static_cast<void*>(thread) << " -> tp=" << tp;
  }

  // The abi.stack_guard slot should start zero and be mutable.
  EXPECT_EQ(thread->abi.stack_guard, 0u);
  thread->abi.stack_guard = 0xdeadbeef;
  EXPECT_EQ(*Launder(&thread->abi.stack_guard), 0xdeadbeefu);
}

// A stack should be accessible and mutable within; guarded below if it grows
// down; guarded above if it grows up.
template <bool GrowsUp = false>
void CheckStack(std::string_view stack_name, PageRoundedSize stack_size, PageRoundedSize guard_size,
                uint64_t* sp) {
  ASSERT_NE(sp, nullptr) << stack_name;

  const size_t stack_words = stack_size.get() / sizeof(uint64_t);
  std::span<uint64_t> stack{sp - (GrowsUp ? 0 : stack_words), stack_words};

  // If zero-initialized and mutable at both ends, probably in the middle too.
  // It could get slow to check every word or even every page.

  EXPECT_EQ(stack[0], 0u) << stack_name;
  *Launder(stack.data()) = 0x1234u;
  EXPECT_EQ(stack[0], 0x1234u) << stack_name;

  EXPECT_EQ(stack.back(), 0u) << stack_name;
  *Launder(&stack.back()) = 0x6789u;
  EXPECT_EQ(stack.back(), 0x6789u) << stack_name;

  // Likewise, if guards fault at both ends, proboably in the middle too.

  uint64_t* in_guard = Launder(GrowsUp ? sp + stack_words : sp - stack_words - 1);
  ASSERT_DEATH(DeathByRead(in_guard), "%s sp=%p in_guard=%p", std::string(stack_name).c_str(), sp,
               in_guard);
  ASSERT_DEATH(DeathByWrite(in_guard), "%s sp=%p in_guard=%p", std::string(stack_name).c_str(), sp,
               in_guard);

  const size_t guard_words = guard_size.get() / sizeof(uint64_t);
  uint64_t* far_in_guard =
      Launder(GrowsUp ? sp + stack_words + guard_words - 1 : stack.data() - guard_words);
  ASSERT_DEATH(DeathByRead(far_in_guard), "%s", std::string(stack_name).c_str());
  ASSERT_DEATH(DeathByWrite(far_in_guard), "%s", std::string(stack_name).c_str());
}

void CheckAllocator(PageRoundedSize stack_size, PageRoundedSize guard_size,
                    const ThreadAllocator& allocator) {
  CheckThread(allocator.thread());

  // The abi.unsafe_sp slot should already be filled in.
  CheckStack("unsafe stack", stack_size, guard_size,
             reinterpret_cast<uint64_t*>(allocator.thread()->abi.unsafe_sp));

  CheckStack("machine stack", stack_size, guard_size, allocator.machine_sp());

  if constexpr (kShadowCallStackAbi) {
    CheckStack<true>("shadow call stack", stack_size, guard_size, allocator.shadow_call_sp());
  } else {
    EXPECT_EQ(allocator.shadow_call_sp(), nullptr);
  }
}

void CheckVmoName(zx::unowned_vmar vmar, std::string_view expected_name, zx_vaddr_t vaddr) {
  // Auto-size the vector for zx_object_get_info.
  constexpr auto get_info = []<typename T>(auto&& handle, zx_object_info_topic_t topic,
                                           std::vector<T>& info) {
    while (true) {
      size_t actual = 0, avail = 0;
      zx_status_t status =
          handle->get_info(topic, info.data(), info.size() * sizeof(T), &actual, &avail);
      info.resize(avail);
      if (status != ZX_ERR_BUFFER_TOO_SMALL) {
        ASSERT_OK(status);
        ASSERT_LE(actual, avail);
        if (actual == avail) {
          return;
        }
      }
    }
  };

  // List all the mappings in the test VMAR.
  std::vector<zx_info_maps_t> maps;
  ASSERT_NO_FATAL_FAILURE(get_info(vmar->borrow(), ZX_INFO_VMAR_MAPS, maps));
  auto by_base = [](const zx_info_maps_t& info) { return info.base; };
  ASSERT_TRUE(std::ranges::is_sorted(maps, std::ranges::less{}, by_base));

  // List all the VMOs used in the process.
  std::vector<zx_info_vmo_t> vmos;
  ASSERT_NO_FATAL_FAILURE(get_info(zx::process::self(), ZX_INFO_PROCESS_VMOS, vmos));

  // Put the VMO info into a map indexed by KOID.
  auto vmos_view = std::views::transform(
      std::views::all(vmos), [](const zx_info_vmo_t& info) -> std::pair<zx_koid_t, zx_info_vmo_t> {
        return {info.koid, info};
      });
  std::map<zx_koid_t, zx_info_vmo_t> vmos_by_koid(vmos_view.begin(), vmos_view.end());

  // Find the mapping covering the vaddr.  It has a KOID for the VMO it maps.
  auto maps_view = std::views::all(maps);
  auto last = std::ranges::upper_bound(maps_view, vaddr, std::ranges::less{}, by_base);
  ASSERT_NE(last, maps_view.begin());
  --last;
  ASSERT_EQ(last->type, ZX_INFO_MAPS_TYPE_MAPPING)
      << std::hex << std::showbase << vaddr << " not found, last at " << last->base;

  auto vmo = vmos_by_koid.find(last->u.mapping.vmo_koid);
  ASSERT_NE(vmo, vmos_by_koid.end());
  const zx_info_vmo_t& vmo_info = vmo->second;
  std::string_view vmo_name{vmo_info.name, std::size(vmo_info.name)};
  vmo_name = vmo_name.substr(0, vmo_name.find_first_of('\0'));
  EXPECT_STREQ(std::string{expected_name}, std::string{vmo_name});
}

TEST_F(LibcThreadTests, ThreadAllocator) {
  ThreadAllocator allocator;

  // Empty when constructed.
  EXPECT_EQ(allocator.thread(), nullptr);
  EXPECT_EQ(allocator.machine_sp(), nullptr);
  EXPECT_EQ(allocator.shadow_call_sp(), nullptr);

  // Allocate the most basic layout: one-page stacks, one-page guards.
  auto result = allocator.Allocate(TestVmar(), kThreadName, kOnePage, kOnePage);
  ASSERT_TRUE(result.is_ok()) << result.status_string();

  CheckVmoName(TestVmar(), kThreadName, reinterpret_cast<uintptr_t>(allocator.thread()));

  CheckAllocator(kOnePage, kOnePage, allocator);
}

TEST_F(LibcThreadTests, ThreadAllocatorDefaultName) {
  ThreadAllocator allocator;

  // Empty when constructed.
  EXPECT_EQ(allocator.thread(), nullptr);
  EXPECT_EQ(allocator.machine_sp(), nullptr);
  EXPECT_EQ(allocator.shadow_call_sp(), nullptr);

  // Use an empty thread name so the non-empty default is used instead.
  auto result = allocator.Allocate(TestVmar(), "", kOnePage, kOnePage);
  ASSERT_TRUE(result.is_ok()) << result.status_string();

  CheckVmoName(TestVmar(), "thread-stacks+TLS", reinterpret_cast<uintptr_t>(allocator.thread()));
}

TEST_F(LibcThreadTests, ThreadAllocatorTooBig) {
  ThreadAllocator allocator;

  // Use a stack size so big that they can't all be mapped in.
  const PageRoundedSize stack{kTestVmarSize / 2};
  auto result = allocator.Allocate(TestVmar(), kThreadName, stack, kOnePage);
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value(), ZX_ERR_NO_RESOURCES);
}

TEST_F(LibcThreadTests, ThreadAllocatorBigStack) {
  ThreadAllocator allocator;

  auto result = allocator.Allocate(TestVmar(), kThreadName, kManyPages, kOnePage);
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  CheckAllocator(kManyPages, kOnePage, allocator);
}

TEST_F(LibcThreadTests, ThreadAllocatorBigGuard) {
  ThreadAllocator allocator;

  auto result = allocator.Allocate(TestVmar(), kThreadName, kOnePage, kManyPages);
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  CheckAllocator(kOnePage, kManyPages, allocator);
}

TEST_F(LibcThreadTests, ThreadAllocatorNoGuard) {
  constexpr PageRoundedSize kNoGuard{};

  ThreadAllocator allocator;

  // Use a tiny test VMAR that only has space for the requested sizes.  If all
  // the blocks fit, then there can't be any guard pages.  Each stack is one
  // page with no guards.  The thread block always gets two one-page guards, so
  // the minimal one is three pages.
  const PageRoundedSize vmar_size = (kOnePage * kStackCount) + (kOnePage * 3);
  auto result = allocator.Allocate(TestVmar(vmar_size), kThreadName, kOnePage, kNoGuard);
  ASSERT_TRUE(result.is_ok()) << result.status_string();
}

TEST_F(LibcThreadTests, ThreadAllocatorTls) {
  // Use a trivial TLS layout as if one TLS module has one uint32_t variable.
  constexpr size_t kTlsStart = TlsTraits::kTlsLocalExecOffset;
  static constexpr TlsLayout kTrivialLayout{
      kTlsStart + sizeof(uint32_t),
      sizeof(uint32_t),
  };
  constexpr ptrdiff_t kTlsBias =
      static_cast<ptrdiff_t>(kTlsStart) -
      (TlsTraits::kTlsNegative ? static_cast<ptrdiff_t>(kTrivialLayout.size_bytes()) : 0);

  gTlsLayout = kTrivialLayout;

  // This both initializes that "variable" and checks that it all started
  // zero-initialized so InitializeTls doesn't need to zero the tbss space.
  constexpr uint32_t kInitValue = 123467890;
  gInitializeTls = [](std::span<std::byte> thread_block, ptrdiff_t tp_offset) {
    ASSERT_LT(tp_offset + kTlsBias, thread_block.size_bytes())
        << " tp_offset " << tp_offset << " + bias " << kTlsBias;
    std::span segment = thread_block.subspan(tp_offset + kTlsBias, sizeof(uint32_t));
    ASSERT_EQ(segment.size_bytes(), sizeof(uint32_t));

    // The segment should be zero-initialized.
    uint32_t* ptr = reinterpret_cast<uint32_t*>(segment.data());
    EXPECT_EQ(*Launder(ptr), 0u);

    *Launder(ptr) = kInitValue;
  };

  ThreadAllocator allocator;
  auto result = allocator.Allocate(TestVmar(), kThreadName, kOnePage, kOnePage);
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  CheckAllocator(kOnePage, kOnePage, allocator);

  void* tp = pthread_to_tp(allocator.thread());
  uint32_t* ptr = ld::TpRelative<uint32_t>(kTlsBias, tp);
  EXPECT_EQ(*Launder(ptr), kInitValue) << "\n    TLS initial data from $tp " << tp << " + bias "
                                       << kTlsBias << " = " << static_cast<void*>(ptr);
  ++*ptr;
  EXPECT_EQ(*Launder(ptr), kInitValue + 1) << "\n    TLS mutated data from $tp " << tp << " + bias "
                                           << kTlsBias << " = " << static_cast<void*>(ptr);
}

TEST_F(LibcThreadTests, ThreadAllocatorTlsAlignment) {
  // Use a layout with the largest supported alignment requirement: one page.
  const TlsLayout kBigAlignmentLayout{17, kOnePage.get()};
  const auto aligned_big = [kBigAlignmentLayout](size_t size) -> size_t {
    return size == 0 ? 0 : kBigAlignmentLayout.Align(size);
  };
  const size_t kAlignedSize = aligned_big(kBigAlignmentLayout.size_bytes());

  gTlsLayout = kBigAlignmentLayout;
  gInitializeTls = [tls_bias =  // Compute the $tp bias for the first module.
                    static_cast<ptrdiff_t>(aligned_big(TlsTraits::kTlsLocalExecOffset)) -
                    static_cast<ptrdiff_t>(TlsTraits::kTlsNegative ? kAlignedSize : 0)](
                       std::span<std::byte> thread_block, ptrdiff_t tp_offset) {
    uintptr_t tp = reinterpret_cast<uintptr_t>(thread_block.data() + tp_offset);
    EXPECT_EQ(0u, (tp + tls_bias) % kOnePage.get())
        << std::hex << std::showbase << "\n    $tp " << tp << " from ["
        << static_cast<void*>(thread_block.data()) << ","
        << static_cast<void*>(thread_block.data() + thread_block.size()) << ") + " << tp_offset
        << "\n        + TLS bias " << tls_bias << " = " << tp + tls_bias << "\n    not aligned to "
        << kOnePage.get();
  };

  ThreadAllocator allocator;
  auto result = allocator.Allocate(TestVmar(), kThreadName, kOnePage, kOnePage);
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  CheckAllocator(kOnePage, kOnePage, allocator);
}

TEST_F(LibcThreadTests, ThreadAllocatorTlsReal) {
  // Use some real TLS data from the executable and an Initial Exec module to
  // ensure things match what the real compiled TLS accesses resolved to and
  // the other tests aren't just matching bugs with the implementation.
  constinit thread_local uint64_t localexec_initial_data = 0x12346789abcdef;
  const ptrdiff_t kLeOffset = ld::TpRelativeToOffset(&localexec_initial_data);
  const ptrdiff_t kIeOffset = ld::TpRelativeToOffset(&tls_dep_data);

  gTlsLayout = ld::testing::gStartupLdAbi.static_tls_layout;
  gInitializeTls = [](std::span<std::byte> thread_block, ptrdiff_t tp_offset) {
    ld::TlsInitialExecDataInit(ld::testing::gStartupLdAbi, thread_block, tp_offset, true);
  };

  auto check_tls = [this, kLeOffset, kIeOffset] {
    // Check basic assumptions about the ambient program state first.
    ASSERT_EQ(*Launder(&localexec_initial_data), 0x12346789abcdef);
    ASSERT_EQ(*Launder(&tls_dep_data), kTlsDepDataValue);
    ASSERT_EQ(*Launder(&tls_dep_bss[0]), '\0');
    ASSERT_EQ(*Launder(&tls_dep_bss[1]), '\0');
    ASSERT_EQ(kLeOffset, ld::TpRelativeToOffset(&localexec_initial_data));
    ASSERT_EQ(kIeOffset, ld::TpRelativeToOffset(&tls_dep_data));

    ASSERT_GE(gTlsLayout.size_bytes(), std::abs(kIeOffset) + sizeof(uint32_t));

    ThreadAllocator allocator;
    auto result = allocator.Allocate(TestVmar(), kThreadName, kOnePage, kOnePage);
    ASSERT_TRUE(result.is_ok()) << result.status_string();
    CheckAllocator(kOnePage, kOnePage, allocator);

    void* tp = pthread_to_tp(allocator.thread());
    EXPECT_EQ(*Launder(ld::TpRelative<uint64_t>(kLeOffset, tp)), 0x12346789abcdef);
    EXPECT_EQ(*Launder(ld::TpRelative<int>(kIeOffset, tp)), kTlsDepDataValue);
    EXPECT_EQ(*Launder(ld::TpRelative<char>(kIeOffset + sizeof(uint32_t), tp)), '\0');
    EXPECT_EQ(*Launder(ld::TpRelative<char>(kIeOffset + sizeof(uint32_t) + 1, tp)), '\0');
  };

  // Do the same check on the initial thread and on a second thread just to be
  // sure the test's own expectations really make sense.
  ASSERT_NO_FATAL_FAILURE(check_tls());
  std::jthread from_other_thread(check_tls);
}

}  // namespace

// This is defined in the non-test code to get the real layout from the dynamic
// linking state and such.  In test code, it's set to a synthetic layout.
TlsLayout ThreadAllocator::GetTlsLayout() {
  // Tests just set this variable beforehand.
  return LibcThreadTests::gTlsLayout;
}

// This is defined in the non-test code to fill the real layout with all the
// actual PT_TLS segments.  In test code, it's a callback set by the test.
void ThreadAllocator::InitializeTls(std::span<std::byte> thread_block, ptrdiff_t tp_offset) {
  if (LibcThreadTests::gInitializeTls) {
    LibcThreadTests::gInitializeTls(thread_block, tp_offset);
  }
}

}  // namespace LIBC_NAMESPACE_DECL
