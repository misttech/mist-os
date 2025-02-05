// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "console.h"

#include <lib/fidl/cpp/wire/client.h>

#include <fbl/string_buffer.h>
#include <zxtest/zxtest.h>

namespace {

// Verify that calling Read() returns data from the RxSource
TEST(ConsoleTestCase, Read) {
  constexpr size_t kReadSize = 10;
  constexpr size_t kWriteCount = kReadSize - 1;
  constexpr uint8_t kWrittenByte = 4;

  sync_completion_t rx_source_done;
  Console::RxSource rx_source = [write_count = kWriteCount,
                                 &rx_source_done](uint8_t* byte) mutable {
    if (write_count == 0) {
      sync_completion_signal(&rx_source_done);
      return ZX_ERR_SHOULD_WAIT;
    }

    *byte = kWrittenByte;
    write_count--;
    return ZX_OK;
  };
  Console::TxSink tx_sink = [](const uint8_t* buffer, size_t length) { return ZX_OK; };

  auto [client_end, server_end] = fidl::Endpoints<fuchsia_hardware_pty::Device>::Create();

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  zx::eventpair event1, event2;
  ASSERT_OK(zx::eventpair::create(0, &event1, &event2));
  Console console(loop.dispatcher(), std::move(event1), std::move(event2), std::move(rx_source),
                  std::move(tx_sink));
  ASSERT_OK(sync_completion_wait_deadline(&rx_source_done, ZX_TIME_INFINITE));
  fidl::BindServer(loop.dispatcher(), std::move(server_end),
                   static_cast<fidl::WireServer<fuchsia_hardware_pty::Device>*>(&console));
  fidl::WireClient client(std::move(client_end), loop.dispatcher());

  client->Read(kReadSize).ThenExactlyOnce(
      [kWriteCount,
       kWrittenByte](fidl::WireUnownedResult<fuchsia_hardware_pty::Device::Read>& result) {
        ASSERT_OK(result.status());
        fit::result response = result.value();
        ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
        ASSERT_EQ(response.value()->data.count(), kWriteCount);
        for (uint8_t byte : response.value()->data) {
          ASSERT_EQ(byte, kWrittenByte);
        }
      });
  ASSERT_OK(loop.RunUntilIdle());
}

// Verify that calling Read() returns data from the RxSource
// and passes through control characters like CTRL+C
// when in RAW mode
TEST(ConsoleTestCase, ReadRaw) {
  constexpr size_t kReadSize = 10;
  constexpr size_t kWriteCount = kReadSize - 1;
  constexpr uint8_t kWrittenByte = 3;

  sync_completion_t rx_source_done;
  sync_completion_t console_setup_done;
  Console::RxSource rx_source = [write_count = kWriteCount, &rx_source_done,
                                 &console_setup_done](uint8_t* byte) mutable {
    sync_completion_wait_deadline(&console_setup_done, ZX_TIME_INFINITE);
    if (write_count == 0) {
      sync_completion_signal(&rx_source_done);
      return ZX_ERR_SHOULD_WAIT;
    }

    *byte = kWrittenByte;
    write_count--;
    return ZX_OK;
  };
  Console::TxSink tx_sink = [](const uint8_t* buffer, size_t length) { return ZX_OK; };

  auto [client_end, server_end] = fidl::Endpoints<fuchsia_hardware_pty::Device>::Create();

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  zx::eventpair event1, event2;
  ASSERT_OK(zx::eventpair::create(0, &event1, &event2));
  Console console(loop.dispatcher(), std::move(event1), std::move(event2), std::move(rx_source),
                  std::move(tx_sink));
  fidl::BindServer(loop.dispatcher(), std::move(server_end),
                   static_cast<fidl::WireServer<fuchsia_hardware_pty::Device>*>(&console));
  fidl::WireClient client(std::move(client_end), loop.dispatcher());
  client->ClrSetFeature(0, fuchsia_hardware_pty::wire::kFeatureRaw)
      .ThenExactlyOnce(
          [](fidl::WireUnownedResult<fuchsia_hardware_pty::Device::ClrSetFeature>& result) {
            ASSERT_EQ(result->features, fuchsia_hardware_pty::wire::kFeatureRaw);
          });
  ASSERT_OK(loop.RunUntilIdle());
  sync_completion_signal(&console_setup_done);
  ASSERT_OK(sync_completion_wait_deadline(&rx_source_done, ZX_TIME_INFINITE));
  client->Read(kReadSize).ThenExactlyOnce(
      [kWriteCount,
       kWrittenByte](fidl::WireUnownedResult<fuchsia_hardware_pty::Device::Read>& result) {
        ASSERT_OK(result.status());
        fit::result response = result.value();
        ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
        ASSERT_EQ(response.value()->data.count(), kWriteCount);
        for (uint8_t byte : response.value()->data) {
          ASSERT_EQ(byte, kWrittenByte);
        }
      });
  ASSERT_OK(loop.RunUntilIdle());
}

// Verify that CTRL+C does the following:
// * Does not write the byte (3) to the RX source
// * Asserts ZX_USER_SIGNAL_1
// * Raises the CTRL+C event.
TEST(ConsoleTestCase, CtrlC) {
  constexpr size_t kReadSize = 2;
  constexpr size_t kWriteCount = kReadSize - 1;
  constexpr uint8_t kWrittenByte = 3;

  sync_completion_t rx_source_done;
  Console::RxSource rx_source = [write_count = kWriteCount,
                                 &rx_source_done](uint8_t* byte) mutable {
    if (write_count == 0) {
      sync_completion_signal(&rx_source_done);
      return ZX_ERR_SHOULD_WAIT;
    }

    *byte = kWrittenByte;
    write_count--;
    return ZX_OK;
  };
  Console::TxSink tx_sink = [](const uint8_t* buffer, size_t length) { return ZX_OK; };

  auto [client_end, server_end] = fidl::Endpoints<fuchsia_hardware_pty::Device>::Create();

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  zx::eventpair event1, event2;
  ASSERT_OK(zx::eventpair::create(0, &event1, &event2));
  Console console(loop.dispatcher(), std::move(event1), std::move(event2), std::move(rx_source),
                  std::move(tx_sink));
  ASSERT_OK(sync_completion_wait_deadline(&rx_source_done, ZX_TIME_INFINITE));
  fidl::BindServer(loop.dispatcher(), std::move(server_end),
                   static_cast<fidl::WireServer<fuchsia_hardware_pty::Device>*>(&console));
  fidl::WireClient client(std::move(client_end), loop.dispatcher());

  // We should NOT receive any data.
  client->Read(kReadSize).ThenExactlyOnce(
      [](fidl::WireUnownedResult<fuchsia_hardware_pty::Device::Read>& result) {
        ASSERT_OK(result.status());
        fit::result response = result.value();
        ASSERT_FALSE(response.is_ok());
        ASSERT_EQ(response.error_value(), ZX_ERR_SHOULD_WAIT);
      });
  ASSERT_OK(loop.RunUntilIdle());
  client->Describe().ThenExactlyOnce(
      [](fidl::WireUnownedResult<fuchsia_hardware_pty::Device::Describe>& result) {
        zx_signals_t pending;
        ASSERT_OK(result->event().wait_one(ZX_USER_SIGNAL_1, zx::time::infinite(), &pending));
        ASSERT_EQ(pending, ZX_USER_SIGNAL_1);
      });
  ASSERT_OK(loop.RunUntilIdle());

  client->ReadEvents().ThenExactlyOnce(
      [](fidl::WireUnownedResult<fuchsia_hardware_pty::Device::ReadEvents>& result) {
        ASSERT_EQ(result->events, fuchsia_hardware_pty::wire::kEventInterrupt);
      });
  ASSERT_OK(loop.RunUntilIdle());
}

TEST(ConsoleTestCase, FeatureBits) {
  constexpr size_t kReadSize = 2;
  constexpr size_t kWriteCount = kReadSize - 1;
  constexpr uint8_t kWrittenByte = 3;

  sync_completion_t rx_source_done;
  Console::RxSource rx_source = [write_count = kWriteCount,
                                 &rx_source_done](uint8_t* byte) mutable {
    if (write_count == 0) {
      sync_completion_signal(&rx_source_done);
      return ZX_ERR_SHOULD_WAIT;
    }

    *byte = kWrittenByte;
    write_count--;
    return ZX_OK;
  };
  Console::TxSink tx_sink = [](const uint8_t* buffer, size_t length) { return ZX_OK; };

  auto [client_end, server_end] = fidl::Endpoints<fuchsia_hardware_pty::Device>::Create();

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  zx::eventpair event1, event2;
  ASSERT_OK(zx::eventpair::create(0, &event1, &event2));
  Console console(loop.dispatcher(), std::move(event1), std::move(event2), std::move(rx_source),
                  std::move(tx_sink));
  ASSERT_OK(sync_completion_wait_deadline(&rx_source_done, ZX_TIME_INFINITE));
  fidl::BindServer(loop.dispatcher(), std::move(server_end),
                   static_cast<fidl::WireServer<fuchsia_hardware_pty::Device>*>(&console));
  fidl::WireClient client(std::move(client_end), loop.dispatcher());

  client->ClrSetFeature(0, 3).ThenExactlyOnce(
      [](fidl::WireUnownedResult<fuchsia_hardware_pty::Device::ClrSetFeature>& result) {
        ASSERT_EQ(result.value().features, 3);
      });
  ASSERT_OK(loop.RunUntilIdle());

  client->ClrSetFeature(1, 0).ThenExactlyOnce(
      [](fidl::WireUnownedResult<fuchsia_hardware_pty::Device::ClrSetFeature>& result) {
        ASSERT_EQ(result.value().features, 2);
      });
  ASSERT_OK(loop.RunUntilIdle());
}

// Verify that calling Write() writes data to the TxSink
TEST(ConsoleTestCase, Write) {
  uint8_t kExpectedBuffer[] = "Hello World";
  size_t kExpectedLength = sizeof(kExpectedBuffer) - 1;

  // Cause the RX thread to exit
  Console::RxSource rx_source = [](uint8_t* byte) { return ZX_ERR_NOT_SUPPORTED; };
  Console::TxSink tx_sink = [kExpectedLength, &kExpectedBuffer](const uint8_t* buffer,
                                                                size_t length) {
    EXPECT_EQ(length, kExpectedLength);
    EXPECT_BYTES_EQ(buffer, kExpectedBuffer, length);
    return ZX_OK;
  };

  auto [client_end, server_end] = fidl::Endpoints<fuchsia_hardware_pty::Device>::Create();

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  zx::eventpair event1, event2;
  ASSERT_OK(zx::eventpair::create(0, &event1, &event2));
  Console console(loop.dispatcher(), std::move(event1), std::move(event2), std::move(rx_source),
                  std::move(tx_sink));
  fidl::BindServer(loop.dispatcher(), std::move(server_end),
                   static_cast<fidl::WireServer<fuchsia_hardware_pty::Device>*>(&console));
  fidl::WireClient client(std::move(client_end), loop.dispatcher());

  client->Write(fidl::VectorView<uint8_t>::FromExternal(kExpectedBuffer, kExpectedLength))
      .ThenExactlyOnce(
          [kExpectedLength](fidl::WireUnownedResult<fuchsia_hardware_pty::Device::Write>& result) {
            ASSERT_OK(result.status());
            fit::result response = result.value();
            ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
            ASSERT_EQ(response.value()->actual_count, kExpectedLength);
          });
  ASSERT_OK(loop.RunUntilIdle());
}

}  // namespace
