// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/devicetree/devicetree.h>
#include <lib/stdcompat/span.h>
#include <lib/uart/all.h>
#include <lib/uart/amlogic.h>
#include <lib/uart/mock.h>
#include <lib/uart/ns8250.h>
#include <lib/uart/null.h>
#include <lib/uart/pl011.h>
#include <lib/uart/uart.h>
#include <lib/zbi-format/driver-config.h>

#include <array>
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

  // Make sure Unparse is instantiated.
  driver.Unparse();

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

auto as_uint8 = [](auto& val) {
  using byte_type = std::conditional_t<std::is_const_v<std::remove_reference_t<decltype(val)>>,
                                       const uint8_t, uint8_t>;
  return cpp20::span<byte_type>(reinterpret_cast<byte_type*>(&val), sizeof(val));
};

auto append = [](auto& vec, auto&& other) { vec.insert(vec.end(), other.begin(), other.end()); };

uint32_t byte_swap(uint32_t val) {
  if constexpr (cpp20::endian::native == cpp20::endian::big) {
    return val;
  } else {
    auto bytes = as_uint8(val);
    return static_cast<uint32_t>(bytes[0]) << 24 | static_cast<uint32_t>(bytes[1]) << 16 |
           static_cast<uint32_t>(bytes[2]) << 8 | static_cast<uint32_t>(bytes[3]);
  }
}

// Small helper so we can verify the behavior of CachedProperties.
struct PropertyBuilder {
  devicetree::Properties Build() {
    return devicetree::Properties(
        {property_block.data(), property_block.size()},
        std::string_view(reinterpret_cast<const char*>(string_block.data()), string_block.size()));
  }

  void Add(std::string_view name, uint32_t value) {
    uint32_t name_off = byte_swap(static_cast<uint32_t>(string_block.size()));
    // String must be null terminated.
    append(string_block, name);
    string_block.push_back('\0');

    uint32_t len = byte_swap(sizeof(uint32_t));

    if (!property_block.empty()) {
      const uint32_t kFdtPropToken = byte_swap(0x00000003);
      append(property_block, as_uint8(kFdtPropToken));
    }
    // this are all 32b aliagned, no padding need.
    append(property_block, as_uint8(len));
    append(property_block, as_uint8(name_off));
    uint32_t be_value = byte_swap(value);
    append(property_block, as_uint8(be_value));
  }

  void Add(std::string_view key, cpp20::span<const std::string_view> str_list) {
    constexpr std::array<uint8_t, sizeof(uint32_t) - 1> kPadding = {};

    uint32_t name_off = byte_swap(static_cast<uint32_t>(string_block.size()));
    // String must be null terminated.
    append(string_block, key);
    string_block.push_back('\0');

    // Add the null terminator.
    uint32_t len = 0;
    for (std::string_view str : str_list) {
      // Include terminator.
      len += str.length() + 1;
    }

    cpp20::span<const uint8_t> padding;
    if (auto remainder = len % sizeof(uint32_t); remainder != 0) {
      padding = cpp20::span(kPadding).subspan(0, sizeof(uint32_t) - remainder);
    }
    len = byte_swap(len);

    if (!property_block.empty()) {
      const uint32_t kFdtPropToken = byte_swap(0x00000003);
      append(property_block, as_uint8(kFdtPropToken));
    }
    append(property_block, as_uint8(len));
    append(property_block, as_uint8(name_off));
    for (auto str : str_list) {
      append(property_block, str);
      property_block.push_back('\0');
    }
    append(property_block, padding);
  }

  std::vector<uint8_t> property_block;
  std::vector<uint8_t> string_block;
};

TEST(UartTests, MatchCompatible) {
  using AllDrivers =
      std::variant<uart::null::Driver, uart::pl011::Driver, uart::ns8250::Mmio32Driver,
                   uart::ns8250::Mmio8Driver, uart::ns8250::LegacyDw8250Driver,
                   uart::ns8250::PioDriver, uart::amlogic::Driver>;
  using AllDriver =
      uart::all::KernelDriver<uart::mock::IoProvider, uart::UnsynchronizedPolicy, AllDrivers>;

  AllDriver driver;

  zbi_dcfg_simple_t dcfg = {
      .mmio_phys = 1,
      .irq = 2,
  };

  auto visit = [](auto&& visitor) {
    auto actual_visitor = [visitor = std::move(visitor)](auto&& driver) {
      using DriverType = std::decay_t<decltype(driver.uart())>;
      if constexpr (!std::is_same_v<uart::null::Driver, DriverType> &&
                    !std::is_same_v<uart::internal::DummyDriver, DriverType>) {
        if constexpr (std::is_same_v<zbi_dcfg_simple_t,
                                     std::decay_t<decltype(driver.uart().config())>>) {
          visitor(driver);
        } else {
          FAIL("Unexpected dcfg_simple_pio_t.");
        }
      } else {
        FAIL("Unexpected uart::null::Driver.");
      }
    };
    return actual_visitor;
  };

  constexpr std::array kCompatibles = {"foo,bar"sv, "ns16550a"sv};
  {
    // Match no reg shift or io width
    PropertyBuilder builder;
    builder.Add("compatible", kCompatibles);
    auto props = builder.Build();
    devicetree::PropertyDecoder decoder(props);

    // Arbitrary range of string views.
    auto emplacer = driver.MatchDevicetree(decoder);
    EXPECT_TRUE(emplacer);
    emplacer(dcfg);

    driver.Visit(visit([&](auto&& driver) {
      EXPECT_EQ(driver.uart().extra(), ZBI_KERNEL_DRIVER_I8250_MMIO8_UART);
      EXPECT_EQ(driver.uart().config_name(), uart::ns8250::Mmio8Driver::config_name());
      EXPECT_EQ(driver.uart().config().mmio_phys, 1);
      EXPECT_EQ(driver.uart().config().irq, 2);
    }));
  }

  // Match with reg shift and io width
  {
    // Match no reg shift or io width
    PropertyBuilder builder;
    builder.Add("compatible", kCompatibles);
    builder.Add("reg-shift", 2);
    builder.Add("reg-io-width", 4);
    auto props = builder.Build();
    devicetree::PropertyDecoder decoder(props);

    // Arbitrary range of string views.
    auto emplacer = driver.MatchDevicetree(decoder);
    EXPECT_TRUE(emplacer);
    emplacer(dcfg);

    driver.Visit(visit([&](auto&& driver) {
      EXPECT_EQ(driver.uart().extra(), ZBI_KERNEL_DRIVER_I8250_MMIO32_UART);
      EXPECT_EQ(driver.uart().config_name(), uart::ns8250::Mmio32Driver::config_name());
      EXPECT_EQ(driver.uart().config().mmio_phys, 1);
      EXPECT_EQ(driver.uart().config().irq, 2);
    }));
  }

  // Match DWLegacy8250
  constexpr std::array kDwCompatibles = {"foo,bar"sv, "snps,dw-apb-uart"sv};
  {
    PropertyBuilder builder;
    builder.Add("compatible", kDwCompatibles);
    auto props = builder.Build();
    devicetree::PropertyDecoder decoder(props);

    // Arbitrary range of string views. Must provide io width and reg shift.
    EXPECT_FALSE(driver.MatchDevicetree(decoder));
  }

  {
    // Match no reg shift or io width
    PropertyBuilder builder;
    builder.Add("compatible", kDwCompatibles);
    builder.Add("reg-shift", 2);
    builder.Add("reg-io-width", 4);
    auto props = builder.Build();
    devicetree::PropertyDecoder decoder(props);

    // Arbitrary range of string views.
    auto emplacer = driver.MatchDevicetree(decoder);
    EXPECT_TRUE(emplacer);
    emplacer(dcfg);

    driver.Visit(visit([&](auto&& driver) {
      EXPECT_EQ(driver.uart().extra(), ZBI_KERNEL_DRIVER_DW8250_UART);
      EXPECT_EQ(driver.uart().config_name(), uart::ns8250::LegacyDw8250Driver::config_name());
      EXPECT_EQ(driver.uart().config().mmio_phys, 1);
      EXPECT_EQ(driver.uart().config().irq, 2);
    }));
  }
}

}  // namespace
