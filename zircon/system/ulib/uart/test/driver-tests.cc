// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/uart/all.h>
#include <lib/uart/mock.h>
#include <lib/uart/ns8250.h>
#include <lib/uart/null.h>
#include <lib/uart/uart.h>
#include <lib/zbi-format/driver-config.h>

#include <string_view>
#include <type_traits>

#include <zxtest/zxtest.h>

using namespace std::literals;

namespace {

TEST(UartTests, Nonblocking) {
  uart::KernelDriver<uart::mock::Driver, uart::mock::IoProvider, uart::mock::SyncPolicy> driver;

  driver.uart()
      .ExpectLock()
      .ExpectInit()
      .ExpectUnlock()
      // First Write call -> sends all chars, no waiting.
      .ExpectLock()
      .ExpectTxReady(true)
      .ExpectWrite("hi!"sv)
      .ExpectUnlock()
      // Second Write call -> sends half, then waits.
      .ExpectLock()
      .ExpectTxReady(true)
      .ExpectWrite("hello "sv)
      .ExpectTxReady(false)
      .ExpectWait(false)
      .ExpectTxReady(true)
      .ExpectWrite("world\r\n"sv)
      .ExpectUnlock();

  driver.Init<uart::mock::Locking>();
  EXPECT_EQ(driver.Write<uart::mock::Locking>("hi!"), 3);
  EXPECT_EQ(driver.Write<uart::mock::Locking>("hello world\n"), 12);
}

TEST(UartTests, LockPolicy) {
  uart::KernelDriver<uart::mock::Driver, uart::mock::IoProvider, uart::mock::SyncPolicy> driver;

  driver.uart()
      .ExpectLock()
      .ExpectInit()
      .ExpectUnlock()
      // First Write call -> sends all chars, no waiting.
      .ExpectTxReady(true)
      .ExpectWrite("hi!"sv)
      // Second Write call -> sends half, then waits.
      .ExpectTxReady(true)
      .ExpectWrite("hello "sv)
      .ExpectTxReady(false)
      .ExpectWait(false)
      .ExpectTxReady(true)
      .ExpectWrite("world\r\n"sv);

  driver.Init<uart::mock::Locking>();
  // Just check that lock args are forwarded correctly.
  EXPECT_EQ(driver.Write<uart::mock::NoopLocking>("hi!"), 3);
  EXPECT_EQ(driver.Write<uart::mock::NoopLocking>("hello world\n"), 12);
}

TEST(UartTests, Blocking) {
  uart::KernelDriver<uart::mock::Driver, uart::mock::IoProvider, uart::mock::SyncPolicy> driver;

  driver.uart()
      .ExpectLock()
      .ExpectInit()
      .ExpectUnlock()
      // First Write call -> sends all chars, no waiting.
      .ExpectLock()
      .ExpectTxReady(true)
      .ExpectWrite("hi!"sv)
      .ExpectUnlock()
      // Second Write call -> sends half, then waits.
      .ExpectLock()
      .ExpectTxReady(true)
      .ExpectWrite("hello "sv)
      .ExpectTxReady(false)
      .ExpectWait(true)
      .ExpectAssertHeld()
      .ExpectEnableTxInterrupt()
      .ExpectTxReady(true)
      .ExpectWrite("world\r\n"sv)
      .ExpectUnlock();

  driver.Init<uart::mock::Locking>();
  EXPECT_EQ(driver.Write<uart::mock::Locking>("hi!"), 3);
  EXPECT_EQ(driver.Write<uart::mock::Locking>("hello world\n"), 12);
}

TEST(UartTests, Null) {
  uart::KernelDriver<uart::null::Driver, uart::mock::IoProvider, uart::UnsynchronizedPolicy> driver;
  // Unsynchronized LockPolicy is dropped.
  driver.Init();
  EXPECT_EQ(driver.Write("hi!"), 3);
  EXPECT_EQ(driver.Write("hello world\n"), 12);
  EXPECT_FALSE(driver.Read());
}

TEST(UartTests, All) {
  using AllDriver = uart::all::KernelDriver<uart::mock::IoProvider, uart::UnsynchronizedPolicy>;

  AllDriver driver;

  // Match against ZBI items to instantiate.
  EXPECT_FALSE(driver.Match(zbi_header_t{}, nullptr));

  // Use selected driver.
  driver.Visit([](auto&& driver) {
    driver.template Init();
    EXPECT_EQ(driver.template Write("hi!"), 3);
  });

  // Transfer state to a new instantiation and pick up using it.
  AllDriver newdriver{driver.uart()};
  newdriver.Visit([](auto&& driver) {
    EXPECT_EQ(driver.template Write("hello world\n"), 12);
    EXPECT_FALSE(driver.template Read());
  });
}

TEST(UartTests, MatchCompatible) {
  using AllDriver = uart::all::KernelDriver<uart::mock::IoProvider, uart::UnsynchronizedPolicy>;

  AllDriver driver;

  zbi_dcfg_simple_t dcfg = {
      .mmio_phys = 1,
      .irq = 2,
  };

  // Arbitrary range of string views.
  EXPECT_TRUE(driver.Match("ns16550", &dcfg));

  driver.Visit([](auto&& driver) {
    using DriverType = std::decay_t<decltype(driver.uart())>;
    if constexpr (!std::is_same_v<uart::null::Driver, DriverType> &&
                  !std::is_same_v<uart::internal::DummyDriver, DriverType>) {
      if constexpr (std::is_same_v<zbi_dcfg_simple_t,
                                   std::decay_t<decltype(driver.uart().config())>>) {
        if (driver.uart().extra() == ZBI_KERNEL_DRIVER_I8250_MMIO_UART) {
          EXPECT_EQ(driver.uart().config_name(), uart::ns8250::MmioDriver::config_name());
        } else {
          EXPECT_EQ(driver.uart().config_name(), uart::ns8250::LegacyDw8250Driver::config_name());
        }
        EXPECT_EQ(driver.uart().config().mmio_phys, 1);
        EXPECT_EQ(driver.uart().config().irq, 2);
      } else {
        FAIL("Unexpected dcfg_simple_pio_t.");
      }
    } else {
      FAIL("Unexpected uart::null::Driver.");
    }
  });
}

}  // namespace
