// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/uart/mock.h>
#include <lib/uart/pl011.h>
#include <lib/uart/uart.h>

#include <cstdint>

#include <zxtest/zxtest.h>

namespace {

using SimpleTestDriver =
    uart::KernelDriver<uart::pl011::Driver, uart::mock::IoProvider, uart::UnsynchronizedPolicy>;
constexpr zbi_dcfg_simple_t kTestConfig = {};

TEST(Pl011Tests, HelloWorld) {
  SimpleTestDriver driver(kTestConfig);

  driver.io()
      .mock()
      .ExpectRead(uint16_t{0b0000'0000'0110'0000}, 0x2C)   // Read state from LCR
      .ExpectWrite(uint16_t{0b0000'0000'0111'0000}, 0x2C)  // Writeback with FIFO enabled
      .ExpectWrite(uint16_t{0b0001'0000'0001}, 0x30)       // Init
      .ExpectRead(uint16_t{0b1000'0000}, 0x18)             // TxReady -> true
      .ExpectWrite(uint16_t{'h'}, 0)                       // Write
      .ExpectRead(uint16_t{0b0000'0000}, 0x18)             // TxReady -> false
      .ExpectRead(uint16_t{0b1000'0000}, 0x18)             // TxReady -> true
      .ExpectWrite(uint16_t{'i'}, 0)                       // Write
      .ExpectRead(uint16_t{0b1000'0000}, 0x18)             // TxReady -> true
      .ExpectWrite(uint16_t{'\r'}, 0)                      // Write
      .ExpectRead(uint16_t{0b1000'0000}, 0x18)             // TxReady -> true
      .ExpectWrite(uint16_t{'\n'}, 0);                     // Write

  driver.Init();
  EXPECT_EQ(3, driver.Write("hi\n"));
}

TEST(Pl011Tests, Read) {
  SimpleTestDriver driver(kTestConfig);

  driver.io()
      .mock()
      .ExpectRead(uint16_t{0b0000'0000'0110'0000}, 0x2C)   // Read state from LCR
      .ExpectWrite(uint16_t{0b0000'0000'0111'0000}, 0x2C)  // Writeback with FIFO enabled
      .ExpectWrite(uint16_t{0b0001'0000'0001}, 0x30)       // Init
      .ExpectRead(uint16_t{0b1000'0000}, 0x18)             // TxReady -> true
      .ExpectWrite(uint16_t{'?'}, 0)                       // Write
      .ExpectRead(uint16_t{0b1000'0000}, 0x18)             // TxReady -> true
      .ExpectWrite(uint16_t{'\r'}, 0)                      // Write
      .ExpectRead(uint16_t{0b1000'0000}, 0x18)             // TxReady -> true
      .ExpectWrite(uint16_t{'\n'}, 0)                      // Write
      .ExpectRead(uint16_t{0b1000'0000}, 0x18)             // Read (rx_fifo_empty)
      .ExpectRead(uint16_t{'q'}, 0)                        // Read (data)
      .ExpectRead(uint16_t{0b1000'0000}, 0x18)             // Read (rx_fifo_empty)
      .ExpectRead(uint16_t{'\r'}, 0);                      // Read (data)

  driver.Init();
  EXPECT_EQ(2, driver.Write("?\n"));
  EXPECT_EQ(uint8_t{'q'}, driver.Read());
  EXPECT_EQ(uint8_t{'\r'}, driver.Read());
}

// Helper for initializing the driver.
void Init(SimpleTestDriver& driver) {
  driver.io()
      .mock()
      .ExpectRead(uint16_t{0b0000'0000'0110'0000}, 0x2C)   // Read state from LCR
      .ExpectWrite(uint16_t{0b0000'0000'0111'0000}, 0x2C)  // Writeback with FIFO enabled
      .ExpectWrite(uint16_t{0b0001'0000'0001}, 0x30);      // Init

  driver.Init();
  driver.io().mock().VerifyAndClear();
}

void InitWithInterrupt(SimpleTestDriver& driver) {
  driver.io()
      .mock()
      .ExpectRead(uint16_t{0b0000'0000'0110'0000}, 0x2C)   // Read state from LCR
      .ExpectWrite(uint16_t{0b0000'0000'0111'0000}, 0x2C)  // Writeback with FIFO enabled
      .ExpectWrite(uint16_t{0b0001'0000'0001}, 0x30)       // Init
      .ExpectWrite<uint16_t>(0b0000'0011'1111'1111, 0x44)  // Clear Interrupts.
      .ExpectWrite<uint16_t>(0b0000, 0x34)                 // Rx and Tx Fifo to 1/8
      .ExpectRead<uint16_t>(0b000, 0x38)                   // Read Interrupt Mask.
      .ExpectWrite<uint16_t>(0b1010000, 0x38)        // Write Interrupt Mask with Rx and Rx Timeout
      .ExpectRead<uint16_t>(0b0001'0000'0001, 0x30)  // Read Control Register State
      .ExpectWrite<uint16_t>(0b0011'0000'0001, 0x30);  // Enable RX.

  driver.Init();
  driver.InitInterrupt();
  driver.io().mock().VerifyAndClear();
}

TEST(Pl011Tests, InitInterrupt) {
  SimpleTestDriver driver(kTestConfig);

  Init(driver);

  // Enable Rx Interrupt.
  driver.io()
      .mock()
      .ExpectWrite<uint16_t>(0b0000'0011'1111'1111, 0x44)  // Clear Interrupts.
      .ExpectWrite<uint16_t>(0b0000, 0x34)                 // Rx and Tx Fifo to 1/8
      .ExpectRead<uint16_t>(0b000, 0x38)                   // Read Interrupt Mask.
      .ExpectWrite<uint16_t>(0b1010000, 0x38)        // Write Interrupt Mask with Rx and Rx Timeout
      .ExpectRead<uint16_t>(0b0001'0000'0001, 0x30)  // Read Control Register State
      .ExpectWrite<uint16_t>(0b0011'0000'0001, 0x30);  // Enable RX.

  driver.InitInterrupt();
}

TEST(Pl011Tests, RxIrqEmptyFifo) {
  SimpleTestDriver driver(kTestConfig);

  Init(driver);

  // Now actual IRQ Handler expectations.
  driver.io()
      .mock()
      .ExpectRead<uint16_t>(0b1'0000, 0x40)   // Read Interrupt Masked Status Register
      .ExpectRead<uint16_t>(0b1'0000, 0x18);  // Check if fifo is empty.

  // Empty Fifo bit is set, so it should just return.

  int call_count = 0;
  driver.Interrupt([](auto& sync, auto& waiter,
                      auto&& disable_tx_irq) { FAIL("Unexpected call on |tx| irq callback."); },
                   [&](auto& sync, auto&& reader, auto&& full) { call_count++; });

  driver.io().mock().VerifyAndClear();
  EXPECT_EQ(call_count, 0);
}

TEST(Pl011Tests, RxIrqWithNonEmptyFifoAndNonFullQueue) {
  SimpleTestDriver driver(kTestConfig);

  InitWithInterrupt(driver);

  // Now actual IRQ Handler expectations.
  driver.io()
      .mock()
      .ExpectRead<uint16_t>(0b1'0000, 0x40)   // Read Interrupt Masked Status Register
      .ExpectRead<uint16_t>(0, 0x18)          // Check if fifo is empty.
      .ExpectRead<uint16_t>(123, 0)           // Read 123 from data register.
      .ExpectRead<uint16_t>(0, 0x18)          // Read flag register
      .ExpectRead<uint16_t>(111, 0)           // Read 111 from data register
      .ExpectRead<uint16_t>(0b1'0000, 0x18);  // Read from flag register, fifo is empty.

  int call_count = 0;
  driver.Interrupt([](auto& sync, auto& waiter,
                      auto&& disable_tx_irq) { FAIL("Unexpected call on |tx| irq callback."); },
                   [&](auto& sync, auto&& reader, auto&& full) {
                     char expected_c = call_count == 0 ? 123 : 111;
                     call_count++;
                     auto c = reader();
                     ASSERT_TRUE(c);
                     EXPECT_EQ(c, expected_c);
                   });

  EXPECT_EQ(call_count, 2);
}

TEST(Pl011Tests, RxTimeoutIrqWithNonEmptyFifoAndNonFullQueue) {
  SimpleTestDriver driver(kTestConfig);

  InitWithInterrupt(driver);

  // Now actual IRQ Handler expectations.
  driver.io()
      .mock()
      .ExpectRead<uint16_t>(0b100'0000, 0x40)  // Read Interrupt Masked Status Register
      .ExpectRead<uint16_t>(0, 0x18)           // Check if fifo is empty.
      .ExpectRead<uint16_t>(123, 0)            // Read 123 from data register.
      .ExpectRead<uint16_t>(0, 0x18)           // Read flag register
      .ExpectRead<uint16_t>(111, 0)            // Read 111 from data register
      .ExpectRead<uint16_t>(0b1'0000, 0x18);   // Read from flag register, fifo is empty.

  int call_count = 0;
  driver.Interrupt([](auto& sync, auto& waiter,
                      auto&& disable_tx_irq) { FAIL("Unexpected call on |tx| irq callback."); },
                   [&](auto& sync, auto&& reader, auto&& full) {
                     char expected_c = call_count == 0 ? 123 : 111;
                     call_count++;
                     auto c = reader();
                     ASSERT_TRUE(c);
                     EXPECT_EQ(c, expected_c);
                   });

  EXPECT_EQ(call_count, 2);
}

TEST(Pl011Tests, RxIrqWithNonEmptyFifoAndFullQueue) {
  SimpleTestDriver driver(kTestConfig);

  InitWithInterrupt(driver);

  // Now actual IRQ Handler expectations.
  driver.io()
      .mock()
      .ExpectRead<uint16_t>(0b1'0000, 0x40)   // Read Interrupt Masked Status Register
      .ExpectRead<uint16_t>(0, 0x18)          // Check if fifo is empty.
      .ExpectRead<uint16_t>(0b1010000, 0x38)  // IMSR Should have Rx and Rx timeout enabled.
      .ExpectWrite<uint16_t>(
          0, 0x38);  // The SW queue is full, so we should disable the RX interrupts.

  int call_count = 0;
  driver.Interrupt([](auto& sync, auto& waiter,
                      auto&& disable_tx_irq) { FAIL("Unexpected call on |tx| irq callback."); },
                   [&](auto& sync, auto&& reader, auto&& full) {
                     full();
                     call_count++;
                   });

  EXPECT_EQ(call_count, 1);
}

TEST(Pl011Tests, TxIrqOnly) {
  SimpleTestDriver driver(kTestConfig);

  InitWithInterrupt(driver);
  driver.io()
      .mock()
      .ExpectRead<uint16_t>(0b00100000, 0x40)  // Read Masked Interrupt Status Register.
      .ExpectRead<uint16_t>(
          0b01110000,
          0x38)  // Read Interrupt Mask Set Clear Register. Eanbled should Be Rx,Tx and Rx Timeout.
      .ExpectWrite<uint16_t>(0b01010000, 0x38);  // Mask the Tx Interrupt.

  int call_count = 0;
  driver.Interrupt(
      [&](auto& sync, auto& waiter, auto&& disable_tx_irq) {
        call_count++;
        disable_tx_irq();
      },
      [](auto& sync, auto&& reader, auto&& full) {
        FAIL("Unexpected call on |tx| irq callback.");
      });

  EXPECT_EQ(call_count, 1);
}
}  // namespace
