// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.buildinfo/cpp/wire.h>
#include <fidl/fuchsia.buildinfo/cpp/wire_test_base.h>
#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.power.statecontrol/cpp/wire.h>
#include <fidl/fuchsia.hardware.power.statecontrol/cpp/wire_test_base.h>
#include <fidl/fuchsia.paver/cpp/wire.h>
#include <fidl/fuchsia.paver/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async/dispatcher.h>
#include <lib/async_patterns/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fastboot/fastboot.h>
#include <lib/fastboot/test/test-transport.h>

#include <future>
#include <unordered_map>
#include <vector>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <zxtest/zxtest.h>

#include "sparse_format.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/lib/storage/vfs/cpp/managed_vfs.h"
#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/service.h"
#include "src/storage/testing/fake-paver.h"

namespace fastboot {

namespace {

void CheckPacketsEqual(const Packets& lhs, const Packets& rhs) {
  ASSERT_EQ(lhs.size(), rhs.size());
  for (size_t i = 0; i < lhs.size(); i++) {
    ASSERT_EQ(lhs[i], rhs[i]);
  }
}

TEST(FastbootTest, NoPacket) {
  Fastboot fastboot(0x40000);
  TestTransport transport;
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  Packets expected_packets = {};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
}

TEST(FastbootTest, GetVarMaxDownloadSize) {
  Fastboot fastboot(0x40000);
  const char command[] = "getvar:max-download-size";
  TestTransport transport;
  transport.AddInPacket(command, strlen(command));
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  Packets expected_packets = {"OKAY0x00040000"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
}

TEST(FastbootTest, GetVarUnknownVariable) {
  Fastboot fastboot(0x40000);
  const char command[] = "getvar:unknown";
  TestTransport transport;
  transport.AddInPacket(command, strlen(command));
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST(FastbootTest, GetVarNotEnoughArgument) {
  Fastboot fastboot(0x40000);
  const char command[] = "getvar";
  TestTransport transport;
  transport.AddInPacket(command, strlen(command));
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST(FastbootTest, UnknownCommand) {
  Fastboot fastboot(0x40000);
  const char command[] = "Unknown";
  TestTransport transport;
  transport.AddInPacket(command, strlen(command));
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

const char kNewLineMessage[] =
    "test line 1\n"
    "test line 2";
const size_t kNewLineMessageSplits = 2ULL;

TEST(FastbootResponseTest, FailNewLineMessageNotSplit) {
  Fastboot fastboot(0x40000);
  std::string_view message(kNewLineMessage);
  TestTransport transport;
  auto res = fastboot.SendResponse(Fastboot::ResponseType::kFail, message, &transport, zx::ok());
  ASSERT_OK(res);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0], std::string("FAIL") + kNewLineMessage);
}

TEST(FastbootResponseTest, OkayNewLineMessageNotSplit) {
  Fastboot fastboot(0x40000);
  std::string_view message(kNewLineMessage);
  TestTransport transport;
  auto res = fastboot.SendResponse(Fastboot::ResponseType::kOkay, message, &transport, zx::ok());
  ASSERT_OK(res);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0], std::string("OKAY") + kNewLineMessage);
}

TEST(FastbootResponseTest, InfoNewLineMessageSplitAtNewLine) {
  Fastboot fastboot(0x40000);
  std::string_view message(kNewLineMessage);
  TestTransport transport;
  auto res = fastboot.SendResponse(Fastboot::ResponseType::kInfo, message, &transport, zx::ok());
  ASSERT_OK(res);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), kNewLineMessageSplits);
  ASSERT_EQ(sent_packets[0], std::string("INFO") + "test line 1");
  ASSERT_EQ(sent_packets[1], std::string("INFO") + "test line 2");
}

const char kLongMessage[] =
    "0123456789abcdef"
    "1123456789abcdef"
    "2123456789abcdef"
    "3123456789abcdef"
    "4123456789abcdef"
    "5123456789abcdef"
    "6123456789abcdef"
    "7123456789abcdef";
const size_t kLongMessageSplits = 3ULL;

TEST(FastbootResponseTest, FailLongMessageNotSplit) {
  Fastboot fastboot(0x40000);
  std::string_view message(kLongMessage);
  TestTransport transport;
  auto res = fastboot.SendResponse(Fastboot::ResponseType::kFail, message, &transport, zx::ok());
  ASSERT_OK(res);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0], (std::string("FAIL") + kLongMessage).substr(0, kMaxCommandPacketSize));
}

TEST(FastbootResponseTest, OkayLongMessageNotSplit) {
  Fastboot fastboot(0x40000);
  std::string_view message(kLongMessage);
  TestTransport transport;
  auto res = fastboot.SendResponse(Fastboot::ResponseType::kOkay, message, &transport, zx::ok());
  ASSERT_OK(res);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0], (std::string("OKAY") + kLongMessage).substr(0, kMaxCommandPacketSize));
}

TEST(FastbootResponseTest, InfoLongMessageSplitAtNewLine) {
  Fastboot fastboot(0x40000);
  std::string_view message(kLongMessage);
  TestTransport transport;
  auto res = fastboot.SendResponse(Fastboot::ResponseType::kInfo, message, &transport, zx::ok());
  ASSERT_OK(res);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), kLongMessageSplits);
  std::string res_message;
  for (auto& pkt : sent_packets) {
    EXPECT_EQ(pkt.compare(0, 4, "INFO"), 0);
    res_message += pkt.substr(4);
  }
  EXPECT_STREQ(res_message, kLongMessage);
}

TEST(FastbootResponseTest, InfoLongMessageErrorAdded) {
  Fastboot fastboot(0x40000);
  const int error = ZX_ERR_NOT_SUPPORTED;
  const std::string error_str = "(ZX_ERR_NOT_SUPPORTED)";
  std::string_view message(kLongMessage);
  TestTransport transport;
  auto res =
      fastboot.SendResponse(Fastboot::ResponseType::kInfo, message, &transport, zx::error(error));
  ASSERT_TRUE(res.is_error());
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), kLongMessageSplits);
  std::string res_message;
  for (auto& pkt : sent_packets) {
    EXPECT_EQ(pkt.compare(0, 4, "INFO"), 0);
    res_message += pkt.substr(4);
  }
  EXPECT_STREQ(res_message, kLongMessage + error_str);
}

// Test to cover error string at the end of the message that can be truncated (it doesn't wrap as
// message)
TEST(FastbootResponseTest, InfoLongMessageErrorAddedTruncated) {
  Fastboot fastboot(0x40000);
  const int error = ZX_ERR_NOT_SUPPORTED;
  const std::string error_str = "(ZX_ERR_";  // NOT_SUPPORTED)"; Truncated error message
  std::string message = std::string(kLongMessage) + "padding                                    ";
  TestTransport transport;
  auto res =
      fastboot.SendResponse(Fastboot::ResponseType::kInfo, message, &transport, zx::error(error));
  ASSERT_TRUE(res.is_error());
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), kLongMessageSplits);
  std::string res_message;
  for (auto& pkt : sent_packets) {
    EXPECT_EQ(pkt.compare(0, 4, "INFO"), 0);
    res_message += pkt.substr(4);
  }
  EXPECT_STREQ(res_message, message + error_str);
}

TEST(FastbootResponseTest, BadType) {
  Fastboot fastboot(0x40000);
  const int expected_error = ZX_ERR_INVALID_ARGS;
  Fastboot::ResponseType bad_type = static_cast<Fastboot::ResponseType>(123);
  TestTransport transport;
  auto res = fastboot.SendResponse(bad_type, {}, &transport, zx::ok());
  ASSERT_TRUE(res.is_error());
  EXPECT_EQ(res.error_value(), expected_error);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 0ULL);
}

}  // namespace

class FastbootDownloadTest : public zxtest::Test {
 public:
  // Exercises the protocol to download the given data to the Fastboot instance.
  static void DownloadData(Fastboot& fastboot, const std::vector<uint8_t>& download_content) {
    std::string size_hex_str = fxl::StringPrintf("%08zx", download_content.size());

    std::string command = "download:" + size_hex_str;
    TestTransport transport;
    transport.AddInPacket(command);
    zx::result<> ret = fastboot.ProcessPacket(&transport);
    ASSERT_OK(ret);
    std::vector<std::string> expected_packets = {
        "DATA" + size_hex_str,
    };
    ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
    ASSERT_EQ(fastboot.download_vmo_mapper_.size(), download_content.size());
    // start the download

    // Transmit the first half.
    std::vector<uint8_t> first_half(download_content.begin(),
                                    download_content.begin() + download_content.size() / 2);
    transport.AddInPacket(first_half);
    ret = fastboot.ProcessPacket(&transport);
    ASSERT_OK(ret);
    // There should be no new response packet.
    ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
    ASSERT_BYTES_EQ(fastboot.download_vmo_mapper_.start(), first_half.data(), first_half.size());

    // Transmit the second half
    std::vector<uint8_t> second_half(download_content.begin() + first_half.size(),
                                     download_content.end());
    transport.AddInPacket(second_half);
    ret = fastboot.ProcessPacket(&transport);
    ASSERT_OK(ret);
    expected_packets.push_back("OKAY");
    ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
    ASSERT_BYTES_EQ(fastboot.download_vmo_mapper_.start(), download_content.data(),
                    download_content.size());
  }

  // Overload for string data; does not include any trailing null.
  static void DownloadData(Fastboot& fastboot, std::string_view content) {
    std::vector<uint8_t> byte_content(content.size());
    memcpy(byte_content.data(), content.data(), content.size());
    DownloadData(fastboot, byte_content);
  }
};

namespace {

TEST_F(FastbootDownloadTest, DownloadSucceed) {
  Fastboot fastboot(0x40000);

  std::vector<uint8_t> download_content;
  for (size_t i = 0; i <= 0xff; i++) {
    download_content.push_back(static_cast<uint8_t>(i));
  }

  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));
}

TEST_F(FastbootDownloadTest, DownloadCompleteResetState) {
  Fastboot fastboot(0x40000);

  std::vector<uint8_t> download_content;
  for (size_t i = 0; i <= std::numeric_limits<uint8_t>::max(); i++) {
    download_content.push_back(static_cast<uint8_t>(i));
  }

  // Test the download command twice. The second time is to test that Fastboot re-enter
  // the command waiting state after a complete download.
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));
  // Make sure that all states are reset.
  ASSERT_EQ(fastboot.remaining_download_size(), 0ULL);
  ASSERT_EQ(fastboot.state(), FastbootBase::State::kCommand);

  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));
}

TEST(FastbootTest, DownloadFailsOnUnexpectedAmountOfData) {
  Fastboot fastboot(0x40000);

  std::vector<uint8_t> download_content;
  for (size_t i = 0; i <= std::numeric_limits<uint8_t>::max(); i++) {
    download_content.push_back(static_cast<uint8_t>(i));
  }

  std::string size_hex_str = fxl::StringPrintf("%08zx", download_content.size());

  std::string command = "download:" + size_hex_str;
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  // Transmit the first half.
  std::vector<uint8_t> first_half(download_content.begin(),
                                  download_content.begin() + download_content.size() / 2);
  transport.AddInPacket(first_half);
  ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  // The second transmit sends the entire download, which will exceed expected size.
  transport.AddInPacket(download_content);
  ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  // Check that the last packet is a FAIL response
  ASSERT_EQ(transport.GetOutPackets().size(), 2ULL);
  ASSERT_EQ(transport.GetOutPackets().back().compare(0, 4, "FAIL"), 0);

  ASSERT_EQ(fastboot.total_download_size(), 0ULL);
  ASSERT_EQ(fastboot.remaining_download_size(), 0ULL);
  ASSERT_EQ(fastboot.state(), FastbootBase::State::kCommand);
}

TEST(FastbootTest, DownloadFailsOnZeroSizeDownload) {
  Fastboot fastboot(0x40000);
  std::string command = "download:00000000";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);

  ASSERT_EQ(fastboot.total_download_size(), 0ULL);
  ASSERT_EQ(fastboot.remaining_download_size(), 0ULL);
  ASSERT_EQ(fastboot.state(), FastbootBase::State::kCommand);
}

TEST(FastbootTest, DownloadFailsOnNotEnoughArgument) {
  Fastboot fastboot(0x40000);
  std::string command = "download";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);

  ASSERT_EQ(fastboot.total_download_size(), 0ULL);
  ASSERT_EQ(fastboot.remaining_download_size(), 0ULL);
  ASSERT_EQ(fastboot.state(), FastbootBase::State::kCommand);
}

class FastbootFailGetDownloadBuffer : public Fastboot {
 public:
  using Fastboot::Fastboot;

 private:
  zx::result<void*> GetDownloadBuffer(size_t total_download_size) override {
    return zx::error(ZX_ERR_UNAVAILABLE);
  }
};

TEST(FastbootTest, DownloadFailsOnetDownloadBuffer) {
  FastbootFailGetDownloadBuffer fastboot(0x40000);

  std::vector<uint8_t> download_content;
  for (size_t i = 0; i <= std::numeric_limits<uint8_t>::max(); i++) {
    download_content.push_back(static_cast<uint8_t>(i));
  }

  std::string size_hex_str = fxl::StringPrintf("%08zx", download_content.size());

  std::string command = "download:" + size_hex_str;
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_TRUE(ret.is_error());

  // Check that the last packet is a FAIL response
  ASSERT_EQ(transport.GetOutPackets().size(), 1ULL);
  ASSERT_EQ(transport.GetOutPackets().back().compare(0, 4, "FAIL"), 0);

  ASSERT_EQ(fastboot.total_download_size(), 0ULL);
  ASSERT_EQ(fastboot.remaining_download_size(), 0ULL);
  ASSERT_EQ(fastboot.state(), FastbootBase::State::kCommand);
}

class TestPaver : public paver_test::FakePaver {
 public:
  void UseBlockDevice(UseBlockDeviceRequestView request,
                      UseBlockDeviceCompleter::Sync& completer) override {
    fidl::BindServer(dispatcher(), std::move(request->data_sink), this);
  }
};

class FastbootFlashTest : public FastbootDownloadTest {
 protected:
  FastbootFlashTest() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread), vfs_(loop_.dispatcher()) {
    // Set up a svc root directory with a paver service entry.
    auto root_dir = fbl::MakeRefCounted<fs::PseudoDir>();
    root_dir->AddEntry(
        fidl::DiscoverableProtocolName<fuchsia_paver::Paver>,
        fbl::MakeRefCounted<fs::Service>([this](fidl::ServerEnd<fuchsia_paver::Paver> request) {
          fake_paver_.Connect(loop_.dispatcher(), std::move(request));
          return ZX_OK;
        }));
    zx::result server_end = fidl::CreateEndpoints(&svc_local_);
    ASSERT_OK(server_end.status_value());
    vfs_.ServeDirectory(root_dir, std::move(server_end.value()));
    loop_.StartThread("fastboot-flash-test-loop");
  }

  fidl::ClientEnd<fuchsia_io::Directory>& svc_chan() { return svc_local_; }

  paver_test::FakePaver& paver() { return fake_paver_; }

  ~FastbootFlashTest() override {
    std::promise<zx_status_t> promise;
    vfs_.Shutdown([&promise](zx_status_t status) { promise.set_value(status); });
    ASSERT_OK(promise.get_future().get());
    loop_.Shutdown();
  }

  void TestFlashBootloader(Fastboot& fastboot, fuchsia_paver::wire::Configuration config,
                           const std::string& type_suffix) {
    std::vector<uint8_t> download_content(256, 1);
    ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));

    paver_test::FakePaver& fake_paver = paver();
    fake_paver.set_expected_payload_size(download_content.size());

    std::unordered_map<fuchsia_paver::wire::Configuration, std::string> config_to_partition = {
        {fuchsia_paver::wire::Configuration::kA, "bootloader_a"},
        {fuchsia_paver::wire::Configuration::kB, "bootloader_b"},
        {fuchsia_paver::wire::Configuration::kRecovery, "bootloader_r"},
    };

    TestTransport transport;
    std::string command = "flash:" + config_to_partition[config] + type_suffix;

    transport.AddInPacket(command);
    zx::result<> ret = fastboot.ProcessPacket(&transport);
    ASSERT_OK(ret);

    std::vector<std::string> expected_packets = {"OKAY"};
    ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
    ASSERT_EQ(fake_paver.last_firmware_config(), config);
  }

  void TestFlashBootloaderNoFirmwareType(Fastboot& fastboot,
                                         fuchsia_paver::wire::Configuration config) {
    fake_paver_.set_supported_firmware_type("");
    ASSERT_NO_FATAL_FAILURE(TestFlashBootloader(fastboot, config, ""));
    ASSERT_EQ(fake_paver_.last_firmware_type(), "");
  }

  void TestFlashBootloaderWithFirmwareType(Fastboot& fastboot,
                                           fuchsia_paver::wire::Configuration config,
                                           const std::string& type) {
    fake_paver_.set_supported_firmware_type(type);
    ASSERT_NO_FATAL_FAILURE(TestFlashBootloader(fastboot, config, ":" + type));
    ASSERT_EQ(fake_paver_.last_firmware_type(), type);
  }

  void TestFlashAsset(const std::string& partition, fuchsia_paver::wire::Configuration config,
                      fuchsia_paver::wire::Asset asset) {
    Fastboot fastboot(0x40000, std::move(svc_chan()));
    std::vector<uint8_t> download_content(256, 1);
    ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));
    paver_test::FakePaver& fake_paver = paver();
    fake_paver.set_expected_payload_size(download_content.size());

    std::string command = "flash:" + partition;
    TestTransport transport;
    transport.AddInPacket(command);
    zx::result<> ret = fastboot.ProcessPacket(&transport);
    ASSERT_OK(ret);

    std::vector<std::string> expected_packets = {"OKAY"};
    ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
    ASSERT_EQ(fake_paver.last_asset_config(), config);
    ASSERT_EQ(fake_paver.last_asset(), asset);
  }

  void TestSetActive(const std::string& slot) {
    Fastboot fastboot(0x40000, std::move(svc_chan()));
    paver().set_abr_supported(true);

    TestTransport transport;
    const std::string command = "set_active:" + slot;
    transport.AddInPacket(command);
    zx::result<> ret = fastboot.ProcessPacket(&transport);
    ASSERT_OK(ret);

    std::vector<std::string> expected_packets = {"OKAY"};
    ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
  }

  async::Loop loop_;
  fs::ManagedVfs vfs_;
  TestPaver fake_paver_;
  fidl::ClientEnd<fuchsia_io::Directory> svc_local_;
};

TEST_F(FastbootFlashTest, FlashFailsOnNotEnoughArguments) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));

  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));

  std::string command = "flash";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, FlashFailsOnUnsupportedPartition) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));

  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));

  std::string command = "flash:unknown-partition";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, FlashBootloaderNoAbrNoFirmwareType) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));

  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));

  paver_test::FakePaver& fake_paver = paver();
  fake_paver.set_expected_payload_size(download_content.size());

  TestTransport transport;
  std::string command = "flash:bootloader";

  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  std::vector<std::string> expected_packets = {"OKAY"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
  ASSERT_EQ(fake_paver.last_firmware_config(), fuchsia_paver::wire::Configuration::kA);
}

TEST_F(FastbootFlashTest, FlashBootloaderNoAbrWithFirmwareType) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));

  paver_test::FakePaver& fake_paver = paver();
  fake_paver.set_expected_payload_size(download_content.size());

  std::string firmware_type = "firmware_type";
  fake_paver.set_supported_firmware_type(firmware_type);

  TestTransport transport;
  std::string command = "flash:bootloader:" + firmware_type;

  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  std::vector<std::string> expected_packets = {"OKAY"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
  ASSERT_EQ(fake_paver.last_firmware_config(), fuchsia_paver::wire::Configuration::kA);
  ASSERT_EQ(fake_paver_.last_firmware_type(), firmware_type);
}

TEST_F(FastbootFlashTest, FlashBooloaderASlot) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  ASSERT_NO_FATAL_FAILURE(
      TestFlashBootloaderNoFirmwareType(fastboot, fuchsia_paver::wire::Configuration::kA));
}

TEST_F(FastbootFlashTest, FlashBooloaderBSlot) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  ASSERT_NO_FATAL_FAILURE(
      TestFlashBootloaderNoFirmwareType(fastboot, fuchsia_paver::wire::Configuration::kB));
}

TEST_F(FastbootFlashTest, FlashBooloaderRSlot) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  ASSERT_NO_FATAL_FAILURE(
      TestFlashBootloaderNoFirmwareType(fastboot, fuchsia_paver::wire::Configuration::kRecovery));
}

TEST_F(FastbootFlashTest, FlashBooloaderASlotWithFirmwareType) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  ASSERT_NO_FATAL_FAILURE(TestFlashBootloaderWithFirmwareType(
      fastboot, fuchsia_paver::wire::Configuration::kA, "firmware_type"));
}

TEST_F(FastbootFlashTest, FlashBooloaderBSlotWithFirmwareType) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  ASSERT_NO_FATAL_FAILURE(TestFlashBootloaderWithFirmwareType(
      fastboot, fuchsia_paver::wire::Configuration::kB, "firmware_type"));
}

TEST_F(FastbootFlashTest, FlashBooloaderRSlotWithFirmwareType) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  ASSERT_NO_FATAL_FAILURE(TestFlashBootloaderWithFirmwareType(
      fastboot, fuchsia_paver::wire::Configuration::kRecovery, "firmware_type"));
}

TEST_F(FastbootFlashTest, FlashBooloaderWriteFail) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));

  // Insert a write firmware error
  paver_test::FakePaver& fake_paver = paver();
  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));
  fake_paver.set_expected_payload_size(0);

  std::string command = "flash:bootloader_a";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_FALSE(ret.is_ok());

  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, FlashBooloaderUnsupportedFirmwareType) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));

  // Insert an unsupported firmware failure
  paver().set_supported_firmware_type("unsupported");

  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));

  std::string command = "flash:bootloader_a";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, FlashFuchsiaEsp) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));

  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));

  paver_test::FakePaver& fake_paver = paver();
  fake_paver.set_expected_payload_size(download_content.size());

  std::string command = "flash:fuchsia-esp";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  std::vector<std::string> expected_packets = {"OKAY"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));

  ASSERT_EQ(fake_paver.last_firmware_config(), fuchsia_paver::wire::Configuration::kA);
  ASSERT_EQ(fake_paver.last_firmware_type(), "");
}

TEST_F(FastbootFlashTest, FlashAssetZirconA) {
  TestFlashAsset("zircon_a", fuchsia_paver::wire::Configuration::kA,
                 fuchsia_paver::wire::Asset::kKernel);
}

TEST_F(FastbootFlashTest, FlashAssetZirconB) {
  TestFlashAsset("zircon_b", fuchsia_paver::wire::Configuration::kB,
                 fuchsia_paver::wire::Asset::kKernel);
}

TEST_F(FastbootFlashTest, FlashAssetZirconR) {
  TestFlashAsset("zircon_r", fuchsia_paver::wire::Configuration::kRecovery,
                 fuchsia_paver::wire::Asset::kKernel);
}

TEST_F(FastbootFlashTest, FlashAssetLegacyZirconA) {
  TestFlashAsset("zircon-a", fuchsia_paver::wire::Configuration::kA,
                 fuchsia_paver::wire::Asset::kKernel);
}

TEST_F(FastbootFlashTest, FlashAssetLegacyZirconB) {
  TestFlashAsset("zircon-b", fuchsia_paver::wire::Configuration::kB,
                 fuchsia_paver::wire::Asset::kKernel);
}

TEST_F(FastbootFlashTest, FlashAssetLegacyZirconR) {
  TestFlashAsset("zircon-r", fuchsia_paver::wire::Configuration::kRecovery,
                 fuchsia_paver::wire::Asset::kKernel);
}

TEST_F(FastbootFlashTest, FlashAssetVerifiedBootMetadataA) {
  TestFlashAsset("vbmeta_a", fuchsia_paver::wire::Configuration::kA,
                 fuchsia_paver::wire::Asset::kVerifiedBootMetadata);
}

TEST_F(FastbootFlashTest, FlashAssetVerifiedBootMetadataB) {
  TestFlashAsset("vbmeta_b", fuchsia_paver::wire::Configuration::kB,
                 fuchsia_paver::wire::Asset::kVerifiedBootMetadata);
}

TEST_F(FastbootFlashTest, FlashAssetVerifiedBootMetadataR) {
  TestFlashAsset("vbmeta_r", fuchsia_paver::wire::Configuration::kRecovery,
                 fuchsia_paver::wire::Asset::kVerifiedBootMetadata);
}

TEST_F(FastbootFlashTest, FlashAssetFail) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));
  // Trigger an internal error by using an incorrect size
  paver().set_expected_payload_size(128);

  std::string command = "flash:zircon_a";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_FALSE(ret.is_ok());
  ASSERT_EQ(transport.GetOutPackets().back().compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, SetActiveSlotA) {
  TestSetActive("a");
  ASSERT_TRUE(paver().abr_data().slot_a.active);
}

TEST_F(FastbootFlashTest, SetActiveSlotB) {
  TestSetActive("b");
  ASSERT_TRUE(paver().abr_data().slot_b.active);
}

TEST_F(FastbootFlashTest, SetActiveInvalidSlot) {
  paver().set_abr_supported(true);
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "set_active:r";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, FlashFVM) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));

  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));

  paver_test::FakePaver& fake_paver = paver();
  fake_paver.set_expected_payload_size(download_content.size());

  std::string command = "flash:fvm.sparse";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  std::vector<std::string> expected_packets = {"OKAY"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
  ASSERT_EQ(fake_paver.GetCommandTrace(),
            std::vector<paver_test::Command>{paver_test::Command::kWriteVolumes});
}

TEST_F(FastbootFlashTest, GetVarVersion) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  const char command[] = "getvar:version";
  TestTransport transport;
  transport.AddInPacket(command, strlen(command));
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  std::vector<std::string> expected_packets = {"OKAY0.4"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
}

TEST_F(FastbootFlashTest, GetVarSlotCount) {
  paver().set_abr_supported(true);
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "getvar:slot-count";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  std::vector<std::string> expected_packets = {"OKAY2"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
}

TEST_F(FastbootFlashTest, GetVarSlotCountAbrNotSupported) {
  paver().set_abr_supported(false);
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "getvar:slot-count";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  std::vector<std::string> expected_packets = {"OKAY1"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
}

TEST_F(FastbootFlashTest, GetVarIsUserspace) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "getvar:is-userspace";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  std::vector<std::string> expected_packets = {"OKAYyes"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
}

// Returns a block device path that looks like a ramdisk. Fastboot GPT functionality should
// always ignore ramdisks.
std::string RamDiskPath(int i) { return std::string(kRamDiskString) + "-" + std::to_string(i); }

// A Fastboot subclass which overrides FindGptDevices() to fake out which block devices
// GptDevicePartitioner finds.
class FastbootFakeGptDevices : public Fastboot {
 public:
  // Additional param |block_topology_paths| is the list of fake GPT block devices paths to find.
  // If unspecified, the default is to present one GPT block device and one GPT ramdisk device.
  FastbootFakeGptDevices(size_t max_download_size, fidl::ClientEnd<fuchsia_io::Directory> svc_root,
                         std::vector<std::string> block_topology_paths = {"block-0",
                                                                          RamDiskPath(0)})
      : Fastboot(max_download_size, std::move(svc_root)),
        block_topology_paths_(std::move(block_topology_paths)) {}

  zx::result<std::vector<paver::GptDevicePartitioner::GptClients>> FindGptDevices() override {
    std::vector<paver::GptDevicePartitioner::GptClients> devices;
    for (auto& ele : block_topology_paths_) {
      // We need to have valid channels for the devices, but it doesn't matter what they are.
      zx::channel block, controller;
      if (zx_status_t status = zx::channel::create(0, &block, &controller); status != ZX_OK) {
        return zx::error(status);
      }
      devices.push_back({
          .topological_path = ele,
          .block = fidl::ClientEnd<fuchsia_hardware_block::Block>(std::move(block)),
          .controller = fidl::ClientEnd<fuchsia_device::Controller>(std::move(controller)),
      });
    }
    return zx::ok(std::move(devices));
  }

 private:
  std::vector<std::string> block_topology_paths_;
};

TEST_F(FastbootFlashTest, OemInitPartitionTables) {
  FastbootFakeGptDevices fastboot(0x40000, std::move(svc_chan()));
  std::string command = "oem init-partition-tables";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  std::vector<std::string> expected_packets = {"OKAY"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));

  ASSERT_EQ(paver().GetCommandTrace(),
            std::vector<paver_test::Command>{paver_test::Command::kInitPartitionTables});
}

TEST_F(FastbootFlashTest, OemInitPartitionTablesNoSuitableDevice) {
  FastbootFakeGptDevices fastboot(0x40000, std::move(svc_chan()), {RamDiskPath(0), RamDiskPath(1)});
  std::string command = "oem init-partition-tables";
  TestTransport transport;
  transport.AddInPacket(command);

  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_TRUE(ret.is_error());

  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, OemInitPartitionTablesMoreThanOneSuitableDevice) {
  FastbootFakeGptDevices fastboot(0x40000, std::move(svc_chan()),
                                  {"block-0", "block-1", RamDiskPath(0)});
  std::string command = "oem init-partition-tables";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_TRUE(ret.is_error());

  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, OemWipePartitionTables) {
  FastbootFakeGptDevices fastboot(0x40000, std::move(svc_chan()));
  std::string command = "oem wipe-partition-tables";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  std::vector<std::string> expected_packets = {"OKAY"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));

  ASSERT_EQ(paver().GetCommandTrace(),
            std::vector<paver_test::Command>{paver_test::Command::kWipePartitionTables});
}

// Sends |command| to |fastboot| over |transport| and asserts that the operation is successful.
void CommandSuccess(Fastboot& fastboot, TestTransport& transport, std::string_view command) {
  transport.AddInPacket(command);
  ASSERT_TRUE(fastboot.ProcessPacket(&transport).is_ok());
  ASSERT_EQ(transport.GetOutPackets().size(), 1ULL);
  ASSERT_EQ(transport.GetOutPackets()[0], "OKAY");
}

// Sends |command| to |fastboot| over |transport| and asserts that the operation fails.
void CommandFailure(Fastboot& fastboot, TestTransport& transport, std::string_view command) {
  transport.AddInPacket(command);
  ASSERT_TRUE(fastboot.ProcessPacket(&transport).is_error());
  ASSERT_EQ(transport.GetOutPackets().size(), 1ULL);
  ASSERT_EQ(transport.GetOutPackets()[0].substr(0, 4), "FAIL");
}

TEST_F(FastbootFlashTest, FlashGptMeta) {
  FastbootFakeGptDevices fastboot(0x40000, std::move(svc_chan()));
  TestTransport transport;

  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, kGptMetaDefault));
  ASSERT_NO_FATAL_FAILURE(CommandSuccess(fastboot, transport, "flash:gpt-meta"));
  ASSERT_EQ(paver().GetCommandTrace(),
            std::vector<paver_test::Command>{paver_test::Command::kInitPartitionTables});
}

TEST_F(FastbootFlashTest, FlashGptMetaNoDownloadFailure) {
  FastbootFakeGptDevices fastboot(0x40000, std::move(svc_chan()));
  TestTransport transport;

  // No data downloaded should cause a failure.
  ASSERT_NO_FATAL_FAILURE(CommandFailure(fastboot, transport, "flash:gpt-meta"));
  ASSERT_TRUE(paver().GetCommandTrace().empty());
}

TEST_F(FastbootFlashTest, FlashGptMetaUnknownContentsFailure) {
  FastbootFakeGptDevices fastboot(0x40000, std::move(svc_chan()));
  TestTransport transport;

  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, "default_plus_invalid_extra_contents"));
  ASSERT_NO_FATAL_FAILURE(CommandFailure(fastboot, transport, "flash:gpt-meta"));
  ASSERT_TRUE(paver().GetCommandTrace().empty());
}

TEST_F(FastbootFlashTest, FlashGptMetaInvalidSlotFailure) {
  FastbootFakeGptDevices fastboot(0x40000, std::move(svc_chan()));
  TestTransport transport;

  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, kGptMetaDefault));
  // The gpt-meta pseudo-partition doesn't support slot suffixes.
  ASSERT_NO_FATAL_FAILURE(CommandFailure(fastboot, transport, "flash:gpt-meta_a"));
  ASSERT_TRUE(paver().GetCommandTrace().empty());
}

class FastbootRebootTest : public zxtest::Test {
 public:
  // |TestState| is shared between |Background| and the main test object.
  class TestState {
   public:
    bool reboot_triggered() const {
      fbl::AutoLock al(&lock_);
      return reboot_triggered_;
    }

    void set_reboot_triggered(bool v) {
      fbl::AutoLock al(&lock_);
      reboot_triggered_ = v;
    }

    bool one_shot_recovery() const {
      fbl::AutoLock al(&lock_);
      return one_shot_recovery_;
    }

    void set_one_shot_recovery(bool v) {
      fbl::AutoLock al(&lock_);
      one_shot_recovery_ = v;
    }

   private:
    bool reboot_triggered_ __TA_GUARDED(lock_) = false;
    bool one_shot_recovery_ __TA_GUARDED(lock_) = false;
    mutable fbl::Mutex lock_;
  };

  class MockFidlServer
      : public fidl::testing::WireTestBase<fuchsia_paver::Paver>,
        public fidl::testing::WireTestBase<fuchsia_paver::BootManager>,
        public fidl::testing::WireTestBase<fuchsia_hardware_power_statecontrol::Admin> {
   public:
    MockFidlServer(async_dispatcher_t* dispatcher, std::shared_ptr<TestState> state)
        : dispatcher_(dispatcher), state_(std::move(state)) {}

    fidl::ProtocolHandler<fuchsia_hardware_power_statecontrol::Admin> PublishAdmin() {
      return admin_bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure);
    }

    fidl::ProtocolHandler<fuchsia_paver::Paver> PublishPaver() {
      return paver_bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure);
    }

   private:
    void Reboot(RebootRequestView request, RebootCompleter::Sync& completer) override {
      state_->set_reboot_triggered(true);
      completer.ReplySuccess();
    }

    void FindBootManager(FindBootManagerRequestView request,
                         FindBootManagerCompleter::Sync& completer) override {
      boot_manager_bindings_.AddBinding(dispatcher_, std::move(request->boot_manager), this,
                                        fidl::kIgnoreBindingClosure);
    }

    void SetOneShotRecovery(SetOneShotRecoveryCompleter::Sync& completer) override {
      state_->set_one_shot_recovery(true);
      completer.ReplySuccess();
    }

    void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
      FAIL("Unexpected call to BuildInfo: %s", name.c_str());
    }

    async_dispatcher_t* dispatcher_;
    std::shared_ptr<TestState> state_;
    fidl::ServerBindingGroup<fuchsia_paver::Paver> paver_bindings_;
    fidl::ServerBindingGroup<fuchsia_hardware_power_statecontrol::Admin> admin_bindings_;
    fidl::ServerBindingGroup<fuchsia_paver::BootManager> boot_manager_bindings_;
  };

  // This class is managed and used from a parallel background |async::Loop|
  // while the main thread runs blocking operations.
  class MockComponent {
   public:
    MockComponent(fidl::ServerEnd<fuchsia_io::Directory> svc,
                  const std::shared_ptr<TestState>& state)
        : dispatcher_(async_get_default_dispatcher()),
          outgoing_(dispatcher_),
          server_(dispatcher_, state) {
      ASSERT_OK(outgoing_.AddUnmanagedProtocol<fuchsia_hardware_power_statecontrol::Admin>(
          server_.PublishAdmin()));
      ASSERT_OK(outgoing_.AddUnmanagedProtocol<fuchsia_paver::Paver>(server_.PublishPaver()));
      ASSERT_EQ(ZX_OK, outgoing_.Serve(std::move(svc)).status_value());
    }

   private:
    async_dispatcher_t* dispatcher_;
    component::OutgoingDirectory outgoing_;
    MockFidlServer server_;
  };

  FastbootRebootTest() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    loop_.StartThread("fastboot-reboot-test-loop");

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);

    zx::result svc_local = component::ConnectAt<fuchsia_io::Directory>(endpoints->client, "svc");
    ASSERT_OK(svc_local);

    svc_local_ = std::move(svc_local.value());
    mock_.emplace(std::move(endpoints->server), state_);
  }

  fidl::ClientEnd<fuchsia_io::Directory>& svc_chan() { return svc_local_; }
  const TestState& state() const { return *state_; }

  async::Loop loop_;
  std::shared_ptr<TestState> state_ = std::make_shared<TestState>();
  fidl::ClientEnd<fuchsia_io::Directory> svc_local_;
  async_patterns::DispatcherBound<MockComponent> mock_{loop_.dispatcher()};
};

TEST_F(FastbootRebootTest, Reboot) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "reboot";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  std::vector<std::string> expected_packets = {"OKAY"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
  ASSERT_TRUE(state().reboot_triggered());
}

TEST_F(FastbootRebootTest, Continue) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "continue";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  // One info message plus one OKAY message
  ASSERT_EQ(transport.GetOutPackets().size(), 2ULL);
  ASSERT_EQ(transport.GetOutPackets().back(), "OKAY");
  ASSERT_TRUE(state().reboot_triggered());
}

TEST_F(FastbootRebootTest, RebootBootloader) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "reboot-bootloader";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  // Two info messages plus one OKAY message
  ASSERT_EQ(transport.GetOutPackets().size(), 3ULL);
  ASSERT_EQ(transport.GetOutPackets().back(), "OKAY");
  ASSERT_TRUE(state().reboot_triggered());
  ASSERT_TRUE(state().one_shot_recovery());
}

TEST_F(FastbootFlashTest, UnknownOemCommand) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "oem unknown";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

class FastbootFshostTest : public FastbootDownloadTest {
 public:
  class TestState {
   public:
    const std::string& data_file_name() const {
      fbl::AutoLock al(&lock_);
      return data_file_name_;
    }

    void set_data_file_name(std::string v) {
      fbl::AutoLock al(&lock_);
      data_file_name_ = std::move(v);
    }

    const std::string& data_file_content() const {
      fbl::AutoLock al(&lock_);
      return data_file_content_;
    }

    void set_data_file_content(std::string v) {
      fbl::AutoLock al(&lock_);
      data_file_content_ = std::move(v);
    }

    uint64_t data_file_vmo_content_size() const {
      fbl::AutoLock al(&lock_);
      return data_file_vmo_content_size_;
    }

    void set_data_file_vmo_content_size(uint64_t v) {
      fbl::AutoLock al(&lock_);
      data_file_vmo_content_size_ = v;
    }

   private:
    std::string data_file_name_ TA_GUARDED(lock_);
    std::string data_file_content_ TA_GUARDED(lock_);
    uint64_t data_file_vmo_content_size_ TA_GUARDED(lock_);

    mutable fbl::Mutex lock_;
  };

  class MockFshostAdmin : public fidl::testing::WireTestBase<fuchsia_fshost::Admin> {
   public:
    explicit MockFshostAdmin(async_dispatcher_t* dispatcher,
                             const std::shared_ptr<TestState>& state)
        : dispatcher_(dispatcher), state_(state) {}

    fidl::ProtocolHandler<fuchsia_fshost::Admin> Publish() {
      return admin_bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure);
    }

   private:
    void WriteDataFile(WriteDataFileRequestView request,
                       WriteDataFileCompleter::Sync& completer) override {
      state_->set_data_file_name(std::string(request->filename.data(), request->filename.size()));
      uint64_t size;
      ASSERT_OK(request->payload.get_size(&size));
      std::string data_file_content;
      data_file_content.resize(size);
      ASSERT_OK(request->payload.read(data_file_content.data(), 0, size));
      state_->set_data_file_content(std::move(data_file_content));
      uint64_t data_file_vmo_content_size;
      ASSERT_OK(request->payload.get_prop_content_size(&data_file_vmo_content_size));
      state_->set_data_file_vmo_content_size(data_file_vmo_content_size);
      completer.ReplySuccess();
    }

    void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
      FAIL("Unexpected call to ControllerImpl: %s", name.c_str());
    }

    async_dispatcher_t* dispatcher_;
    std::shared_ptr<TestState> state_;
    fidl::ServerBindingGroup<fuchsia_fshost::Admin> admin_bindings_;
  };

  class MockComponent {
   public:
    explicit MockComponent(fidl::ServerEnd<fuchsia_io::Directory> directory,
                           const std::shared_ptr<TestState>& state)
        : dispatcher_(async_get_default_dispatcher()),
          outgoing_(dispatcher_),
          server_(dispatcher_, state) {
      ASSERT_OK(outgoing_.AddUnmanagedProtocol<fuchsia_fshost::Admin>(server_.Publish()));
      ASSERT_OK(outgoing_.Serve(std::move(directory)));
    }

   private:
    async_dispatcher_t* dispatcher_;
    component::OutgoingDirectory outgoing_;
    MockFshostAdmin server_;
  };

  FastbootFshostTest() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    loop_.StartThread("fastboot-fshost-test-loop");
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);

    zx::result svc_local = component::ConnectAt<fuchsia_io::Directory>(endpoints->client, "svc");
    ASSERT_OK(svc_local);

    svc_local_ = std::move(svc_local.value());
    mock_.emplace(std::move(endpoints->server), state_);
  }

  fidl::ClientEnd<fuchsia_io::Directory>& svc_chan() { return svc_local_; }

  const TestState& state() const { return *state_; }

 private:
  async::Loop loop_;
  std::shared_ptr<TestState> state_ = std::make_shared<TestState>();
  fidl::ClientEnd<fuchsia_io::Directory> svc_local_;
  async_patterns::DispatcherBound<MockComponent> mock_{loop_.dispatcher()};
};

TEST_F(FastbootFshostTest, OemAddStagedBootloaderFile) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));

  std::string command =
      "oem add-staged-bootloader-file " + std::string(sshd_host::kAuthorizedKeysBootloaderFileName);
  TestTransport transport;

  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  std::vector<std::string> expected_packets = {"OKAY"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));

  ASSERT_EQ(state().data_file_name(), sshd_host::kAuthorizedKeyPathInData);
  ASSERT_EQ(state().data_file_vmo_content_size(), download_content.size());
  ASSERT_BYTES_EQ(state().data_file_content().data(), download_content.data(),
                  download_content.size());
}

TEST_F(FastbootFlashTest, OemAddStagedBootloaderFileInvalidNumberOfArguments) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "oem add-staged-bootloader-file";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, OemAddStagedBootloaderFileUnsupportedFile) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::string command = "oem add-staged-bootloader-file unknown";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, FlashRawFVM) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));
  paver_test::FakePaver& fake_paver = paver();
  fake_paver.set_expected_payload_size(download_content.size());

  std::string command = "flash:fvm";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  std::vector<std::string> expected_packets = {"OKAY"};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
}

TEST_F(FastbootFlashTest, FlashRawFVMFail) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  std::vector<uint8_t> download_content(256, 1);
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));
  paver_test::FakePaver& fake_paver = paver();

  // Use an incorrect size to trigger an error
  fake_paver.set_expected_payload_size(download_content.size() + 1);

  std::string command = "flash:fvm";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_TRUE(ret.is_error());

  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST_F(FastbootFlashTest, AndroidSparseImageNotSupported) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  sparse_header_t header{
      .magic = SPARSE_HEADER_MAGIC,
  };
  const uint8_t* header_ptr = reinterpret_cast<const uint8_t*>(&header);
  std::vector<uint8_t> download_content(header_ptr, header_ptr + sizeof(header));
  ASSERT_NO_FATAL_FAILURE(DownloadData(fastboot, download_content));
  paver_test::FakePaver& fake_paver = paver();
  fake_paver.set_expected_payload_size(download_content.size());

  std::string command = "flash:fvm";
  TestTransport transport;
  transport.AddInPacket(command);
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);

  const std::vector<std::string>& sent_packets = transport.GetOutPackets();
  ASSERT_EQ(sent_packets.size(), 1ULL);
  ASSERT_EQ(sent_packets[0].compare(0, 4, "FAIL"), 0);
}

TEST(FastbootBase, ExtractCommandArgsMultipleArgs) {
  FastbootBase::CommandArgs args;
  FastbootBase::ExtractCommandArgs("cmd:arg1:arg2:arg3:a", ":", args);

  EXPECT_EQ(args.num_args, 5);
  EXPECT_EQ(args.args[0], "cmd");
  EXPECT_EQ(args.args[1], "arg1");
  EXPECT_EQ(args.args[2], "arg2");
  EXPECT_EQ(args.args[3], "arg3");
  EXPECT_EQ(args.args[4], "a");
  EXPECT_EQ(args.args[5], "");
}

TEST(FastbootBase, ExtractCommandArgsNoArgs) {
  FastbootBase::CommandArgs args;
  FastbootBase::ExtractCommandArgs("cmd", ":", args);

  EXPECT_EQ(args.num_args, 1);
  EXPECT_EQ(args.args[0], "cmd");
  EXPECT_EQ(args.args[1], "");
}

TEST(FastbootBase, ExtractCommandArgsMiddleEmptyArgs) {
  FastbootBase::CommandArgs args;
  FastbootBase::ExtractCommandArgs("cmd::arg2", ":", args);

  EXPECT_EQ(args.num_args, 2);
  EXPECT_EQ(args.args[0], "cmd");
  EXPECT_EQ(args.args[1], "arg2");
}

TEST(FastbootBase, ExtractCommandArgsEndEmptyArgs) {
  FastbootBase::CommandArgs args;
  FastbootBase::ExtractCommandArgs("cmd:arg1:", ":", args);

  EXPECT_EQ(args.num_args, 2);
  EXPECT_EQ(args.args[0], "cmd");
  EXPECT_EQ(args.args[1], "arg1");
}

TEST(FastbootBase, ExtractCommandArgsMultipleBySpace) {
  FastbootBase::CommandArgs args;
  FastbootBase::ExtractCommandArgs("cmd arg1 arg2 arg3", " ", args);

  EXPECT_EQ(args.num_args, 4);
  EXPECT_EQ(args.args[0], "cmd");
  EXPECT_EQ(args.args[1], "arg1");
  EXPECT_EQ(args.args[2], "arg2");
  EXPECT_EQ(args.args[3], "arg3");
  EXPECT_EQ(args.args[4], "");
}

constexpr char kTestBoardConfig[] = "test-board-config";

class FastbootBuildInfoTest : public FastbootDownloadTest {
 public:
  class MockBuildInfoProvider : public fidl::testing::WireTestBase<fuchsia_buildinfo::Provider> {
   public:
    explicit MockBuildInfoProvider(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

    fidl::ProtocolHandler<fuchsia_buildinfo::Provider> Publish() {
      return provider_bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure);
    }

   private:
    void GetBuildInfo(GetBuildInfoCompleter::Sync& completer) override {
      fidl::Arena arena;
      auto res = fuchsia_buildinfo::wire::BuildInfo::Builder(arena)
                     .board_config(fidl::StringView(kTestBoardConfig))
                     .Build();
      completer.Reply(res);
    }

    void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
      FAIL("Unexpected call to BuildInfo: %s", name.c_str());
    }

    async_dispatcher_t* dispatcher_;
    fidl::ServerBindingGroup<fuchsia_buildinfo::Provider> provider_bindings_;
  };

  class MockComponent {
   public:
    explicit MockComponent(fidl::ServerEnd<fuchsia_io::Directory> server_end)
        : dispatcher_(async_get_default_dispatcher()),
          outgoing_(dispatcher_),
          provider_server_(dispatcher_) {
      ASSERT_OK(
          outgoing_.AddUnmanagedProtocol<fuchsia_buildinfo::Provider>(provider_server_.Publish()));
      ASSERT_OK(outgoing_.Serve(std::move(server_end)));
    }

   private:
    async_dispatcher_t* dispatcher_;
    component::OutgoingDirectory outgoing_;
    MockBuildInfoProvider provider_server_;
  };

  FastbootBuildInfoTest() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    loop_.StartThread("fastboot-build-info-test-loop");
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);

    zx::result svc_local = component::ConnectAt<fuchsia_io::Directory>(endpoints->client, "svc");
    ASSERT_OK(svc_local);

    svc_local_ = std::move(svc_local.value());
    mock_.emplace(std::move(endpoints->server));
  }

  fidl::ClientEnd<fuchsia_io::Directory>& svc_chan() { return svc_local_; }

 private:
  async::Loop loop_;
  fidl::ClientEnd<fuchsia_io::Directory> svc_local_;
  async_patterns::DispatcherBound<MockComponent> mock_{loop_.dispatcher()};
};

TEST_F(FastbootBuildInfoTest, GetVarHwRevision) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  const char command[] = "getvar:hw-revision";
  TestTransport transport;
  transport.AddInPacket(command, strlen(command));
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  Packets expected_packets = {"OKAY" + std::string(kTestBoardConfig)};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
}

TEST_F(FastbootBuildInfoTest, GetVarProduct) {
  Fastboot fastboot(0x40000, std::move(svc_chan()));
  const char command[] = "getvar:product";
  TestTransport transport;
  transport.AddInPacket(command, strlen(command));
  zx::result<> ret = fastboot.ProcessPacket(&transport);
  ASSERT_OK(ret);
  Packets expected_packets = {"OKAY" + std::string(kTestBoardConfig)};
  ASSERT_NO_FATAL_FAILURE(CheckPacketsEqual(transport.GetOutPackets(), expected_packets));
}

}  // namespace

}  // namespace fastboot
