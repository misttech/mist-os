// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bt_hci_broadcom.h"

#include <assert.h>
#include <endian.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/random.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/threads.h>

namespace bt_hci_broadcom {

constexpr uint32_t kTargetBaudRate = 2000000;
constexpr uint32_t kDefaultBaudRate = 115200;

constexpr zx::duration kFirmwareDownloadDelay = zx::msec(50);

// Hardcoded. Better to parameterize on chipset. Broadcom chips need a few hundred msec delay after
// firmware load.
constexpr zx::duration kBaudRateSwitchDelay = zx::msec(200);

void BtHciBroadcom::GetFeatures(GetFeaturesCompleter::Sync& completer) {
  completer.Reply(fuchsia_hardware_bluetooth::BtVendorFeatures::kSetAclPriorityCommand);
}

void BtHciBroadcom::EncodeCommand(EncodeCommandRequestView request,
                                  EncodeCommandCompleter::Sync& completer) {
  uint8_t data_buffer[kBcmSetAclPriorityCmdSize];
  switch (request->command.Which()) {
    case fuchsia_hardware_bluetooth::wire::BtVendorCommand::Tag::kSetAclPriority: {
      EncodeSetAclPriorityCommand(request->command.set_acl_priority(), data_buffer);
      auto encoded_cmd =
          fidl::VectorView<uint8_t>::FromExternal(data_buffer, kBcmSetAclPriorityCmdSize);
      completer.ReplySuccess(encoded_cmd);
      return;
    }
    default: {
      completer.ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }
  }
}

void BtHciBroadcom::OpenHci(OpenHciCompleter::Sync& completer) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_bluetooth::Hci>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Failed to create endpoints: %s", zx_status_get_string(endpoints.error_value()));
    completer.ReplyError(endpoints.error_value());
    return;
  }
  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(endpoints->server),
                   this);
  completer.ReplySuccess(std::move(endpoints->client));
}

void BtHciBroadcom::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(ERROR, "Unknown method in Vendor request, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

const std::unordered_map<uint16_t, std::string> BtHciBroadcom::kFirmwareMap = {
    {PDEV_PID_BCM43458, "BCM4345C5.hcd"},
    {PDEV_PID_BCM4359, "BCM4359C0.hcd"},
};

BtHciBroadcom::BtHciBroadcom(zx_device_t* parent, async_dispatcher_t* dispatcher)
    : BtHciBroadcomType(parent), dispatcher_(dispatcher) {}
zx_status_t BtHciBroadcom::Create(void* ctx, zx_device_t* parent) {
  return Create(ctx, parent, /*dispatcher=*/nullptr);
}

zx_status_t BtHciBroadcom::Create(void* ctx, zx_device_t* parent, async_dispatcher_t* dispatcher) {
  std::unique_ptr<BtHciBroadcom> dev = std::make_unique<BtHciBroadcom>(parent, dispatcher);

  zx_status_t bind_status = dev->Bind();
  if (bind_status != ZX_OK) {
    return bind_status;
  }

  // Driver Manager is now in charge of the device.
  // Memory will be explicitly freed in DdkRelease().
  [[maybe_unused]] BtHciBroadcom* unused = dev.release();
  return ZX_OK;
}

void BtHciBroadcom::DdkInit(ddk::InitTxn txn) {
  init_txn_.emplace(std::move(txn));

  // Spawn a new thread in production. In tests, use the test dispatcher provided in the
  // constructor.
  // Note that the fdf default dispatcher is used at all other places in this driver, this new
  // thread is only created for running the executor. The reason is that the tasks running on this
  // executor fire synchronous FIDL calls, if we put the executor on the same dispatcher as the FIDL
  // client which is used to fire sync FIDL call, there'll be a re-entrancy issue and result in a
  // deadlock.
  // TODO(b/303116559): Creating a new thread is not encouraged in a driver because it'll be
  // unmanaged by driver framework. Create a dispatcher instead.
  if (!dispatcher_) {
    loop_.emplace(&kAsyncLoopConfigNoAttachToCurrentThread);
    zx_status_t status = loop_->StartThread("bt-hci-broadcom-init");
    if (status != ZX_OK) {
      zxlogf(ERROR, "failed to start init thread: %s", zx_status_get_string(status));
      OnInitializeComplete(status);
      return;
    }
    dispatcher_ = loop_->dispatcher();
  }
  executor_.emplace(dispatcher_);

  // Continue initialization in the new thread.
  executor_->schedule_task(Initialize());
}

// NOLINTNEXTLINE(readability-convert-member-functions-to-static)
void BtHciBroadcom::DdkUnbind(ddk::UnbindTxn txn) { txn.Reply(); }

void BtHciBroadcom::DdkRelease() {
  command_channel_.reset();

  // Driver manager is given a raw pointer to this dynamically allocated object in Create(), so
  // when DdkRelease() is called we need to free the allocated memory.
  delete this;
}

void BtHciBroadcom::OpenCommandChannel(OpenCommandChannelRequestView request,
                                       OpenCommandChannelCompleter::Sync& completer) {
  hci_client_->OpenCommandChannel(std::move(request->channel))
      .ThenExactlyOnce(
          [completer = completer.ToAsync()](
              fidl::WireUnownedResult<fuchsia_hardware_bluetooth::Hci::OpenCommandChannel>&
                  result) mutable {
            if (!result.ok()) {
              zxlogf(ERROR, "OpenCommandChannel failed with FIDL error %s", result.status_string());
              completer.ReplyError(result.status());
              return;
            }
            if (result->is_error()) {
              zxlogf(ERROR, "OpenCommandChannel failed with error %s",
                     zx_status_get_string(result->error_value()));
              completer.ReplyError(result->error_value());
              return;
            }
            completer.ReplySuccess();
          });
}

void BtHciBroadcom::OpenAclDataChannel(OpenAclDataChannelRequestView request,
                                       OpenAclDataChannelCompleter::Sync& completer) {
  hci_client_->OpenAclDataChannel(std::move(request->channel))
      .ThenExactlyOnce(
          [completer = completer.ToAsync()](
              fidl::WireUnownedResult<fuchsia_hardware_bluetooth::Hci::OpenAclDataChannel>&
                  result) mutable {
            if (!result.ok()) {
              zxlogf(ERROR, "OpenAclDataChannel failed with FIDL error %s", result.status_string());
              completer.ReplyError(result.status());
              return;
            }
            if (result->is_error()) {
              zxlogf(ERROR, "OpenAclDataChannel failed with error %s",
                     zx_status_get_string(result->error_value()));
              completer.ReplyError(result->error_value());
              return;
            }
            completer.ReplySuccess();
          });
}
void BtHciBroadcom::OpenScoDataChannel(OpenScoDataChannelRequestView request,
                                       OpenScoDataChannelCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void BtHciBroadcom::ConfigureSco(ConfigureScoRequestView request,
                                 ConfigureScoCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void BtHciBroadcom::ResetSco(ResetScoCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void BtHciBroadcom::OpenIsoDataChannel(OpenIsoDataChannelRequestView request,
                                       OpenIsoDataChannelCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void BtHciBroadcom::OpenSnoopChannel(OpenSnoopChannelRequestView request,
                                     OpenSnoopChannelCompleter::Sync& completer) {
  hci_client_->OpenSnoopChannel(std::move(request->channel))
      .ThenExactlyOnce(
          [completer = completer.ToAsync()](
              fidl::WireUnownedResult<fuchsia_hardware_bluetooth::Hci::OpenSnoopChannel>&
                  result) mutable {
            if (!result.ok()) {
              zxlogf(ERROR, "OpenSnoopChannel failed with FIDL error %s", result.status_string());
              completer.ReplyError(result.status());
              return;
            }
            if (result->is_error()) {
              zxlogf(ERROR, "OpenSnoopChannel failed with error %s",
                     zx_status_get_string(result->error_value()));
              completer.ReplyError(result->error_value());
              return;
            }
            completer.ReplySuccess();
          });
}

void BtHciBroadcom::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(ERROR, "Unknown method in Hci request, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t BtHciBroadcom::ConnectToHciFidlProtocol() {
  zx::result<fidl::ClientEnd<fuchsia_hardware_bluetooth::Hci>> client_end =
      DdkConnectFidlProtocol<fuchsia_hardware_bluetooth::HciService::Hci>();
  if (client_end.is_error()) {
    zxlogf(ERROR, "Connect to Hci Fidl protocol failed: %s", client_end.status_string());
    return client_end.status_value();
  }

  hci_client_ =
      fidl::WireClient(*std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());
  return ZX_OK;
}

zx_status_t BtHciBroadcom::ConnectToSerialFidlProtocol() {
  zx::result<fdf::ClientEnd<fuchsia_hardware_serialimpl::Device>> client_end =
      DdkConnectRuntimeProtocol<fuchsia_hardware_serialimpl::Service::Device>();
  if (client_end.is_error()) {
    zxlogf(ERROR, "Connect to Serial FIDL protocol failed: %s", client_end.status_string());
    return client_end.status_value();
  }

  serial_client_ = fdf::WireSyncClient(*std::move(client_end));
  return ZX_OK;
}

void BtHciBroadcom::EncodeSetAclPriorityCommand(
    fuchsia_hardware_bluetooth::wire::BtVendorSetAclPriorityParams params, void* out_buffer) {
  BcmSetAclPriorityCmd command = {
      .header =
          {
              .opcode = htole16(kBcmSetAclPriorityCmdOpCode),
              .parameter_total_size = sizeof(BcmSetAclPriorityCmd) - sizeof(HciCommandHeader),
          },
      .connection_handle = htole16(params.connection_handle),
      .priority = (params.priority == fuchsia_hardware_bluetooth::BtVendorAclPriority::kNormal)
                      ? kBcmAclPriorityNormal
                      : kBcmAclPriorityHigh,
      .direction = (params.direction == fuchsia_hardware_bluetooth::BtVendorAclDirection::kSource)
                       ? kBcmAclDirectionSource
                       : kBcmAclDirectionSink,
  };

  memcpy(out_buffer, &command, sizeof(command));
}

fpromise::promise<std::vector<uint8_t>, zx_status_t> BtHciBroadcom::SendCommand(const void* command,
                                                                                size_t length) {
  // send HCI command
  zx_status_t status = command_channel_.write(/*flags=*/0, command, static_cast<uint32_t>(length),
                                              /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    zxlogf(ERROR, "command channel write failed %s", zx_status_get_string(status));
    return fpromise::make_result_promise<std::vector<uint8_t>, zx_status_t>(
        fpromise::error(status));
  }

  return ReadEvent();
}

fpromise::promise<std::vector<uint8_t>, zx_status_t> BtHciBroadcom::ReadEvent() {
  return executor_
      ->MakePromiseWaitHandle(zx::unowned_handle(command_channel_.get()),
                              ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED)
      .then([this](fpromise::result<zx_packet_signal_t, zx_status_t>&)
                -> fpromise::result<std::vector<uint8_t>, zx_status_t> {
        std::vector<uint8_t> read_buf(kChanReadBufLen, 0u);
        uint32_t actual = 0;
        zx_status_t status = command_channel_.read(
            /*flags=*/0, read_buf.data(), /*handles=*/nullptr, kChanReadBufLen,
            /*num_handles=*/0, &actual, /*actual_handles=*/nullptr);
        if (status != ZX_OK) {
          return fpromise::error(status);
        }

        if (actual < sizeof(HciCommandComplete)) {
          zxlogf(ERROR, "command channel read too short: %d < %lu", actual,
                 sizeof(HciCommandComplete));
          return fpromise::error(ZX_ERR_INTERNAL);
        }

        HciCommandComplete event;
        std::memcpy(&event, read_buf.data(), sizeof(HciCommandComplete));
        if (event.header.event_code != kHciEvtCommandCompleteEventCode ||
            event.header.parameter_total_size < kMinEvtParamSize) {
          zxlogf(ERROR, "did not receive command complete or params too small");
          return fpromise::error(ZX_ERR_INTERNAL);
        }

        if (event.return_code != 0) {
          zxlogf(ERROR, "got command complete error %u", event.return_code);
          return fpromise::error(ZX_ERR_INTERNAL);
        }

        read_buf.resize(actual);
        return fpromise::ok(std::move(read_buf));
      });
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::SetBaudRate(uint32_t baud_rate) {
  BcmSetBaudRateCmd command = {
      .header =
          {
              .opcode = kBcmSetBaudRateCmdOpCode,
              .parameter_total_size = sizeof(BcmSetBaudRateCmd) - sizeof(HciCommandHeader),
          },
      .unused = 0,
      .baud_rate = htole32(baud_rate),
  };

  return SendCommand(&command, sizeof(command))
      .and_then(
          [this, baud_rate](const std::vector<uint8_t>&) -> fpromise::result<void, zx_status_t> {
            fdf::Arena arena('CONF');
            auto result = serial_client_.buffer(arena)->Config(
                baud_rate, fuchsia_hardware_serialimpl::wire::kSerialSetBaudRateOnly);
            if (!result.ok()) {
              return fpromise::error(result.status());
            }
            if (result->is_error()) {
              return fpromise::error(result->error_value());
            }
            return fpromise::ok();
          });
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::SetBdaddr(
    const std::array<uint8_t, kMacAddrLen>& bdaddr) {
  BcmSetBdaddrCmd command = {
      .header =
          {
              .opcode = kBcmSetBdaddrCmdOpCode,
              .parameter_total_size = sizeof(BcmSetBdaddrCmd) - sizeof(HciCommandHeader),
          },
      .bdaddr =
          {// HCI expects little endian. Swap bytes
           bdaddr[5], bdaddr[4], bdaddr[3], bdaddr[2], bdaddr[1], bdaddr[0]},
  };

  return SendCommand(&command.header, sizeof(command)).and_then([](std::vector<uint8_t>&) {});
}

fpromise::result<std::array<uint8_t, kMacAddrLen>, zx_status_t>
BtHciBroadcom::GetBdaddrFromBootloader() {
  std::array<uint8_t, kMacAddrLen> mac_addr;
  size_t actual_len;
  zx_status_t status = device_get_metadata(parent(), DEVICE_METADATA_MAC_ADDRESS, mac_addr.data(),
                                           sizeof(mac_addr), &actual_len);
  if (status != ZX_OK) {
    return fpromise::error(status);
  }
  if (actual_len < kMacAddrLen) {
    return fpromise::error(ZX_ERR_INTERNAL);
  }
  zxlogf(INFO, "got bootloader mac address %02x:%02x:%02x:%02x:%02x:%02x", mac_addr[0], mac_addr[1],
         mac_addr[2], mac_addr[3], mac_addr[4], mac_addr[5]);

  return fpromise::ok(mac_addr);
}

fpromise::promise<> BtHciBroadcom::LogControllerFallbackBdaddr() {
  return SendCommand(&kReadBdaddrCmd, sizeof(kReadBdaddrCmd))
      .then([](fpromise::result<std::vector<uint8_t>, zx_status_t>& result) {
        char fallback_addr[18] = "<unknown>";

        if (result.is_ok() && sizeof(ReadBdaddrCommandComplete) == result.value().size()) {
          ReadBdaddrCommandComplete event;
          std::memcpy(&event, result.value().data(), result.value().size());
          // HCI returns data as little endian. Swap bytes
          snprintf(fallback_addr, sizeof(fallback_addr), "%02x:%02x:%02x:%02x:%02x:%02x",
                   event.bdaddr[5], event.bdaddr[4], event.bdaddr[3], event.bdaddr[2],
                   event.bdaddr[1], event.bdaddr[0]);
        }

        zxlogf(ERROR, "error getting mac address from bootloader: %s. Fallback address: %s.",
               zx_status_get_string(result.is_ok() ? ZX_OK : result.error()), fallback_addr);
      });
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::LoadFirmware() {
  zx::vmo fw_vmo;
  size_t fw_size;

  // If there's no firmware for this PID, we don't expect the bind to happen without a
  // corresponding entry in the firmware table. Please double-check the PID value and add an entry
  // to the firmware table if it's valid.
  ZX_ASSERT_MSG(kFirmwareMap.find(serial_pid_) != kFirmwareMap.end(), "no mapping for PID: %u",
                serial_pid_);
  zx_status_t status = load_firmware(zxdev(), kFirmwareMap.at(serial_pid_).c_str(),
                                     fw_vmo.reset_and_get_address(), &fw_size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "no firmware file found");
    return fpromise::make_error_promise(status);
  }

  return SendCommand(&kStartFirmwareDownloadCmd, sizeof(kStartFirmwareDownloadCmd))
      .or_else([](zx_status_t& status) -> fpromise::result<std::vector<uint8_t>, zx_status_t> {
        zxlogf(ERROR, "could not load firmware file");
        return fpromise::error(status);
      })
      .and_then([this](std::vector<uint8_t>& /*event*/) mutable {
        // give time for placing firmware in download mode
        return executor_->MakeDelayedPromise(zx::duration(kFirmwareDownloadDelay))
            .then([](fpromise::result<>& /*result*/) {
              return fpromise::result<void, zx_status_t>(fpromise::ok());
            });
      })
      .and_then([this, fw_vmo = std::move(fw_vmo), fw_size]() mutable {
        // The firmware is a sequence of HCI commands containing the firmware data as payloads.
        return SendVmoAsCommands(std::move(fw_vmo), fw_size, /*offset=*/0);
      })
      .and_then([this]() -> fpromise::promise<void, zx_status_t> {
        if (is_uart_) {
          // firmware switched us back to 115200. switch back to kTargetBaudRate.
          fdf::Arena arena('CONF');
          auto result = serial_client_.buffer(arena)->Config(
              kDefaultBaudRate, fuchsia_hardware_serialimpl::wire::kSerialSetBaudRateOnly);
          if (!result.ok()) {
            return fpromise::make_result_promise(fpromise::error(result.status()));
          }
          if (result->is_error()) {
            return fpromise::make_result_promise(fpromise::error(result->error_value()));
          }

          return executor_->MakeDelayedPromise(kBaudRateSwitchDelay)
              .then(
                  [this](fpromise::result<>& /*result*/) { return SetBaudRate(kTargetBaudRate); });
        }
        return fpromise::make_result_promise<void, zx_status_t>(fpromise::ok());
      })
      .and_then([]() { zxlogf(INFO, "firmware loaded"); });
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::SendVmoAsCommands(zx::vmo vmo, size_t size,
                                                                      size_t offset) {
  if (offset == size) {
    return fpromise::make_result_promise<void, zx_status_t>(fpromise::ok());
  }

  uint8_t buffer[kMaxHciCommandSize];

  size_t remaining = size - offset;
  size_t read_amount = (remaining > sizeof(buffer) ? sizeof(buffer) : remaining);

  if (read_amount < sizeof(HciCommandHeader)) {
    zxlogf(ERROR, "short HCI command in firmware download");
    return fpromise::make_error_promise(ZX_ERR_INTERNAL);
  }

  zx_status_t status = vmo.read(buffer, offset, read_amount);
  if (status != ZX_OK) {
    return fpromise::make_error_promise(status);
  }

  HciCommandHeader header;
  std::memcpy(&header, buffer, sizeof(HciCommandHeader));
  size_t length = header.parameter_total_size + sizeof(header);
  if (read_amount < length) {
    zxlogf(ERROR, "short HCI command in firmware download");
    return fpromise::make_error_promise(ZX_ERR_INTERNAL);
  }

  offset += length;

  return SendCommand(buffer, length)
      .then([this, vmo = std::move(vmo), size,
             offset](fpromise::result<std::vector<uint8_t>, zx_status_t>& result) mutable
            -> fpromise::promise<void, zx_status_t> {
        if (result.is_error()) {
          zxlogf(ERROR, "SendCommand failed in firmware download: %s",
                 zx_status_get_string(result.error()));
          return fpromise::make_error_promise<zx_status_t>(result.error());
        }

        // Send the next command
        return SendVmoAsCommands(std::move(vmo), size, offset);
      });
}

fpromise::promise<void> BtHciBroadcom::Initialize() {
  zx::channel theirs;
  zx_status_t status = zx::channel::create(/*flags=*/0, &command_channel_, &theirs);
  if (status != ZX_OK) {
    OnInitializeComplete(status);
    return fpromise::make_error_promise();
  }

  zxlogf(DEBUG, "opening command channel");
  auto result = hci_client_.sync()->OpenCommandChannel(std::move(theirs));
  if (!result.ok()) {
    zxlogf(ERROR, "OpenCommandChannel failed FIDL error: %s", result.status_string());
    OnInitializeComplete(status);
    return fpromise::make_error_promise();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "OpenCommandChannel failed : %s", zx_status_get_string(result->error_value()));
    OnInitializeComplete(status);
    return fpromise::make_error_promise();
  }

  zxlogf(DEBUG, "sending initial reset command");
  return SendCommand(&kResetCmd, sizeof(kResetCmd))
      .and_then([this](std::vector<uint8_t>&) -> fpromise::promise<void, zx_status_t> {
        if (is_uart_) {
          zxlogf(DEBUG, "setting baud rate to %u", kTargetBaudRate);
          // switch baud rate to TARGET_BAUD_RATE
          return SetBaudRate(kTargetBaudRate);
        }
        return fpromise::make_result_promise<void, zx_status_t>(fpromise::ok());
      })
      .and_then([this]() {
        zxlogf(DEBUG, "loading firmware");
        return LoadFirmware();
      })
      .and_then([this]() {
        zxlogf(DEBUG, "sending reset command");
        return SendCommand(&kResetCmd, sizeof(kResetCmd));
      })
      .and_then([this](std::vector<uint8_t>&) -> fpromise::promise<void, zx_status_t> {
        zxlogf(DEBUG, "setting BDADDR to value from bootloader");
        fpromise::result<std::array<uint8_t, kMacAddrLen>, zx_status_t> bdaddr =
            GetBdaddrFromBootloader();

        if (bdaddr.is_error()) {
          return LogControllerFallbackBdaddr().then(
              [](fpromise::result<>&) -> fpromise::result<void, zx_status_t> {
                return fpromise::ok();
              });
        }

        // send Set BDADDR command
        return SetBdaddr(bdaddr.value());
      })
      .then([this](fpromise::result<void, zx_status_t>& result) {
        zx_status_t status = result.is_ok() ? ZX_OK : result.error();
        OnInitializeComplete(status);
      });
}

void BtHciBroadcom::OnInitializeComplete(zx_status_t status) {
  // We're done with the command channel. Close it so that it can be opened by
  // the host stack after the device becomes visible.
  if (command_channel_.is_valid()) {
    zxlogf(DEBUG, "closing command channel");
    command_channel_.reset();
  }

  if (status == ZX_OK) {
    zxlogf(INFO, "initialization completed successfully");
  } else {
    zxlogf(ERROR, "device initialization failed: %s", zx_status_get_string(status));
  }

  // In production, the initialization loop/thread is no longer needed.
  if (loop_) {
    loop_->Quit();
  }

  init_txn_->Reply(status);
}

zx_status_t BtHciBroadcom::Bind() {
  zx_status_t status = ConnectToHciFidlProtocol();
  if (status != ZX_OK) {
    zxlogf(ERROR, "ConnectToHciFidlProtocol failed: %s", zx_status_get_string(status));
    return status;
  }

  status = ConnectToSerialFidlProtocol();
  if (status != ZX_OK) {
    zxlogf(ERROR, "ConnectToSerialFidlProtocol failed: %s", zx_status_get_string(status));
  } else {
    is_uart_ = true;
  }
  fdf::Arena arena('INFO');
  auto result = serial_client_.buffer(arena)->GetInfo();
  if (!result.ok()) {
    zxlogf(ERROR, "GetInfo failed FIDL error: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "GetInfo failed : %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  serial_pid_ = result.value()->info.serial_pid;

  ddk::DeviceAddArgs args("bt-hci-broadcom");
  args.set_proto_id(ZX_PROTOCOL_BT_HCI);
  args.set_flags(DEVICE_ADD_NON_BINDABLE);
  return DdkAdd(args);
}

static zx_driver_ops_t bcm_hci_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = BtHciBroadcom::Create,
};

}  // namespace bt_hci_broadcom

ZIRCON_DRIVER(bcm_hci, bt_hci_broadcom::bcm_hci_driver_ops, "zircon", "0.1");
