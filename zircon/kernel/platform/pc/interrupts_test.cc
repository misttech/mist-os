// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/unittest/unittest.h>

#include <arch/x86/fake_msr_access.h>
#include <arch/x86/pv.h>
#include <ktl/iterator.h>
#include <ktl/unique_ptr.h>
#include <vm/physmap.h>

#include "interrupt_manager.h"

#include <ktl/enforce.h>

namespace {

class FakeIoApic {
 private:
  // Make sure we can have more interrupts than CPU vectors, so we can test
  // too many allocations
  static constexpr unsigned int kIrqCount = X86_INT_COUNT + 1;

 public:
  static bool IsValidInterrupt(unsigned int vector, uint32_t flags) { return vector < kIrqCount; }
  static uint8_t FetchIrqVector(unsigned int vector) {
    ZX_ASSERT(vector < kIrqCount);
    return FakeIoApic::entries[vector].x86_vector;
  }
  static void ConfigureIrqVector(uint32_t global_irq, uint8_t x86_vector) {
    ZX_ASSERT(global_irq < kIrqCount);
    FakeIoApic::entries[global_irq].x86_vector = x86_vector;
  }
  static void ConfigureIrq(uint32_t global_irq, enum interrupt_trigger_mode trig_mode,
                           enum interrupt_polarity polarity,
                           enum apic_interrupt_delivery_mode del_mode, bool mask,
                           enum apic_interrupt_dst_mode dst_mode, uint8_t dst, uint8_t vector) {
    ZX_ASSERT(global_irq < kIrqCount);
    FakeIoApic::entries[global_irq].x86_vector = vector;
    FakeIoApic::entries[global_irq].trig_mode = trig_mode;
    FakeIoApic::entries[global_irq].polarity = polarity;
  }
  static void MaskIrq(uint32_t global_irq, bool mask) { ZX_ASSERT(global_irq < kIrqCount); }
  static zx_status_t FetchIrqConfig(uint32_t global_irq, enum interrupt_trigger_mode* trig_mode,
                                    enum interrupt_polarity* polarity) {
    ZX_ASSERT(global_irq < kIrqCount);
    *trig_mode = FakeIoApic::entries[global_irq].trig_mode;
    *polarity = FakeIoApic::entries[global_irq].polarity;
    return ZX_OK;
  }

  // Reset the internal state
  static void Reset() {
    for (auto& entry : entries) {
      entry = {};
    }
  }

  struct Entry {
    uint8_t x86_vector;
    enum interrupt_trigger_mode trig_mode;
    enum interrupt_polarity polarity;
  };
  static Entry entries[kIrqCount];
};
FakeIoApic::Entry FakeIoApic::entries[kIrqCount] = {};

bool TestRegisterInterruptHandler() {
  BEGIN_TEST;

  FakeIoApic::Reset();
  fbl::AllocChecker ac;
  auto im = ktl::make_unique<InterruptManager<FakeIoApic>>(&ac);
  ASSERT_TRUE(ac.check());
  ASSERT_EQ(im->Init(), ZX_OK);

  unsigned int kIrq1 = 1;
  interrupt_handler_t handler1 = []() { return; };

  // Register a handler for the interrupt
  ASSERT_EQ(im->RegisterInterruptHandler(kIrq1, ktl::move(handler1)), ZX_OK);
  EXPECT_NE(FakeIoApic::entries[kIrq1].x86_vector, 0);

  // Unregister it
  ASSERT_EQ(im->RegisterInterruptHandler(kIrq1, nullptr), ZX_OK);
  EXPECT_EQ(FakeIoApic::entries[kIrq1].x86_vector, 0);

  END_TEST;
}

bool TestRegisterInterruptHandlerTwice() {
  BEGIN_TEST;

  FakeIoApic::Reset();
  fbl::AllocChecker ac;
  auto im = ktl::make_unique<InterruptManager<FakeIoApic>>(&ac);
  ASSERT_TRUE(ac.check());
  ASSERT_EQ(im->Init(), ZX_OK);

  unsigned int kIrq = 1;

  interrupt_handler_t handler1 = []() { return; };
  interrupt_handler_t handler2 = []() { return; };

  ASSERT_EQ(im->RegisterInterruptHandler(kIrq, ktl::move(handler1)), ZX_OK);
  uint8_t irq_x86_vector = FakeIoApic::entries[kIrq].x86_vector;
  ASSERT_EQ(im->RegisterInterruptHandler(kIrq, ktl::move(handler2)), ZX_ERR_ALREADY_BOUND);
  ASSERT_EQ(irq_x86_vector, FakeIoApic::entries[kIrq].x86_vector);

  // Unregister it
  ASSERT_EQ(im->RegisterInterruptHandler(kIrq, nullptr), ZX_OK);
  EXPECT_EQ(FakeIoApic::entries[kIrq].x86_vector, 0);

  END_TEST;
}

bool TestUnregisterInterruptHandlerNotRegistered() {
  BEGIN_TEST;

  FakeIoApic::Reset();
  fbl::AllocChecker ac;
  auto im = ktl::make_unique<InterruptManager<FakeIoApic>>(&ac);
  ASSERT_TRUE(ac.check());
  ASSERT_EQ(im->Init(), ZX_OK);

  unsigned int kIrq1 = 1;

  // Unregister a vector we haven't yet registered.  Should just be ignored.
  ASSERT_EQ(im->RegisterInterruptHandler(kIrq1, nullptr), ZX_OK);

  END_TEST;
}

bool TestRegisterInterruptHandlerTooMany() {
  BEGIN_TEST;

  FakeIoApic::Reset();
  fbl::AllocChecker ac;
  auto im = ktl::make_unique<InterruptManager<FakeIoApic>>(&ac);
  ASSERT_TRUE(ac.check());
  ASSERT_EQ(im->Init(), ZX_OK);

  static_assert(ktl::size(FakeIoApic::entries) > InterruptManager<FakeIoApic>::kNumCpuVectors);

  // Register every interrupt, and store different pointers for them so we
  // can validate it.  All of these should succeed, but will exhaust the
  // allocator.
  for (unsigned i = 0; i < InterruptManager<FakeIoApic>::kNumCpuVectors; ++i) {
    interrupt_handler_t handler = []() { return; };
    ASSERT_EQ(im->RegisterInterruptHandler(i, ktl::move(handler)), ZX_OK);
  }

  // Try to allocate one more
  interrupt_handler_t handler = []() { return; };
  ASSERT_EQ(im->RegisterInterruptHandler(InterruptManager<FakeIoApic>::kNumCpuVectors,
                                         ktl::move(handler)),
            ZX_ERR_NO_RESOURCES);

  // Clean up the registered handlers
  for (unsigned i = 0; i < InterruptManager<FakeIoApic>::kNumCpuVectors; ++i) {
    ASSERT_EQ(im->RegisterInterruptHandler(i, nullptr), ZX_OK);
  }

  END_TEST;
}

}  // namespace

// This test isn't in the namespace so that the InterruptManager can friend it.
bool TestHandlerAllocationAlignment() TA_NO_THREAD_SAFETY_ANALYSIS {
  BEGIN_TEST;

  fbl::AllocChecker ac;
  auto im = ktl::make_unique<InterruptManager<FakeIoApic>>(&ac);
  ASSERT_TRUE(ac.check());
  ASSERT_EQ(im->Init(), ZX_OK);

  uint base = 0;

  // Allocation in new IM should succeed and be correctly aligned.
  zx_status_t status = im->AllocHandler(32, &base);
  EXPECT_EQ(status, ZX_OK);
  EXPECT_EQ(base % 32, 0u);
  im->FreeHandler(base, 32);

  // Set a high bit such that our allocation just won't fit.
  im->handler_allocated_.Set(X86_INT_PLATFORM_BASE + 31, X86_INT_PLATFORM_BASE + 31 + 1);
  status = im->AllocHandler(32, &base);
  EXPECT_EQ(status, ZX_OK);
  EXPECT_GT(base, X86_INT_PLATFORM_BASE + 31u);
  EXPECT_EQ(base % 32, 0u);
  im->FreeHandler(base, 32);
  im->FreeHandler(X86_INT_PLATFORM_BASE + 31, 1);

  // Set a low bit ensuring that allocation happens on the next roundup up block.
  im->handler_allocated_.Set(X86_INT_PLATFORM_BASE, X86_INT_PLATFORM_BASE + 1);
  status = im->AllocHandler(32, &base);
  EXPECT_EQ(status, ZX_OK);
  EXPECT_EQ(base % 32, 0u);
  im->FreeHandler(base, 32);
  im->FreeHandler(X86_INT_PLATFORM_BASE, 1);

  // Set two bits such that the distance between them is greater than our desired allocation
  // but such that the only valid alignment requires an allocation in an even higher block.
  im->handler_allocated_.Set(X86_INT_PLATFORM_BASE, X86_INT_PLATFORM_BASE + 1);
  im->handler_allocated_.Set(X86_INT_PLATFORM_BASE + 34, X86_INT_PLATFORM_BASE + 34 + 1);
  status = im->AllocHandler(32, &base);
  EXPECT_EQ(status, ZX_OK);
  EXPECT_GT(base, X86_INT_PLATFORM_BASE + 34u);
  EXPECT_EQ(base % 32, 0u);
  im->FreeHandler(base, 32);
  im->FreeHandler(X86_INT_PLATFORM_BASE, 1);
  im->FreeHandler(X86_INT_PLATFORM_BASE + 34, 1);

  END_TEST;
}

namespace {

bool TestPvEoi() {
  BEGIN_TEST;

  // Can't signal if not enabled.
  {
    PvEoi pv;
    pv.Init();
    EXPECT_FALSE(pv.Eoi());
  }

  // Enable, signal, then disable.
  {
    FakeMsrAccess msr{};
    msr.msrs_[0] = {X86_MSR_KVM_PV_EOI_EN, 0U};

    PvEoi pv;
    pv.Init();
    pv.Enable(&msr);

    // Find the PvEoi's state via MSR value.
    uint64_t pa = msr.msrs_[0].value;
    EXPECT_TRUE(pa & X86_MSR_KVM_PV_EOI_EN_ENABLE);
    pa &= ~X86_MSR_KVM_PV_EOI_EN_ENABLE;
    auto state = reinterpret_cast<uint64_t*>(paddr_to_physmap(pa));

    EXPECT_EQ(*state, 0U);

    EXPECT_FALSE(pv.Eoi());
    EXPECT_EQ(*state, 0U);

    *state = 1;
    EXPECT_TRUE(pv.Eoi());
    EXPECT_EQ(*state, 0U);

    pv.Disable(&msr);
    EXPECT_EQ(msr.msrs_[0].value, 0U);
  }

  END_TEST;
}

}  // namespace

UNITTEST_START_TESTCASE(pc_interrupt_tests)
UNITTEST("RegisterInterruptHandler", TestRegisterInterruptHandler)
UNITTEST("RegisterInterruptHandlerTwice", TestRegisterInterruptHandlerTwice)
UNITTEST("UnregisterInterruptHandlerNotRegistered", TestUnregisterInterruptHandlerNotRegistered)
UNITTEST("RegisterInterruptHandlerTooMany", TestRegisterInterruptHandlerTooMany)
UNITTEST("HandlerAllocationAlignment", TestHandlerAllocationAlignment)
UNITTEST("PvEoi", TestPvEoi)
UNITTEST_END_TESTCASE(pc_interrupt_tests, "pc_interrupt", "Tests for external interrupts")
