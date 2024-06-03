// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_LOOPBACK_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_LOOPBACK_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/driver/devfs/cpp/connector.h>

#include "bt-hci.h"

namespace bt_hci_virtual {

using AddChildCallback = fit::function<void(fuchsia_driver_framework::wire::NodeAddArgs)>;

class LoopbackDevice : public fidl::WireServer<fuchsia_hardware_bluetooth::Hci>,
                       public fidl::WireServer<fuchsia_hardware_bluetooth::Vendor> {
 public:
  explicit LoopbackDevice();

  // Methods to control the LoopbackDevice's lifecycle. These are used by the VirtualController.
  zx_status_t Initialize(zx_handle_t channel, std::string_view name, AddChildCallback callback);
  void Shutdown();

 private:
  // fuchsia_hardware_bluetooth::Vendor protocol interface implementations.
  void EncodeCommand(EncodeCommandRequestView request,
                     EncodeCommandCompleter::Sync& completer) override;
  void OpenHci(OpenHciCompleter::Sync& completer) override;
  void OpenHciTransport(OpenHciTransportCompleter::Sync& completer) override {}
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // fuchsia_hardware_bluetooth::Hci protocol interface implementations.
  void OpenCommandChannel(OpenCommandChannelRequestView request,
                          OpenCommandChannelCompleter::Sync& completer) override;
  void OpenAclDataChannel(OpenAclDataChannelRequestView request,
                          OpenAclDataChannelCompleter::Sync& completer) override;
  void OpenScoDataChannel(OpenScoDataChannelRequestView request,
                          OpenScoDataChannelCompleter::Sync& completer) override;
  void ConfigureSco(ConfigureScoRequestView request,
                    ConfigureScoCompleter::Sync& completer) override;
  void ResetSco(ResetScoCompleter::Sync& completer) override;
  void OpenIsoDataChannel(OpenIsoDataChannelRequestView request,
                          OpenIsoDataChannelCompleter::Sync& completer) override;
  void OpenSnoopChannel(OpenSnoopChannelRequestView request,
                        OpenSnoopChannelCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  void Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request);

  // HCI UART packet indicators
  enum BtHciPacketIndicator : uint8_t {
    kHciNone = 0,
    kHciCommand = 1,
    kHciAclData = 2,
    kHciSco = 3,
    kHciEvent = 4,
  };

  // This wrapper around async_wait enables us to get a LoopbackDevice* in the handler.
  // We use this instead of async::WaitMethod because async::WaitBase isn't thread safe.
  struct Wait : public async_wait {
    explicit Wait(LoopbackDevice* uart, zx::channel* channel);
    static void Handler(async_dispatcher_t* dispatcher, async_wait_t* async_wait,
                        zx_status_t status, const zx_packet_signal_t* signal);
    LoopbackDevice* uart;
    // Indicates whether a wait has begun and not ended.
    bool pending = false;
    // The channel that this wait waits on.
    zx::channel* channel;
  };

  // Returns length of current event packet being received.
  // Must only be called in the read callback (HciHandleUartReadEvents).
  size_t EventPacketLength();

  // Returns length of current ACL data packet being received.
  // Must only be called in the read callback (HciHandleUartReadEvents).
  size_t AclPacketLength();

  // Returns length of current SCO data packet being received.
  // Must only be called in the read callback (HciHandleUartReadEvents).
  size_t ScoPacketLength();

  void ChannelCleanup(zx::channel* channel);

  void SnoopChannelWrite(uint8_t flags, uint8_t* bytes, size_t length);

  void HciBeginShutdown();

  void HciHandleIncomingChannel(zx::channel* chan, zx_signals_t pending);

  void HciHandleClientChannel(zx::channel* chan, zx_signals_t pending);

  // Reads the next packet chunk from |uart_src| into |buffer| and increments |buffer_offset| and
  // |uart_src| by the number of bytes read.
  // If a complete packet is read, it will be written to |channel|.
  using PacketLengthFunction = size_t (LoopbackDevice::*)();
  void ProcessNextUartPacketFromReadBuffer(uint8_t* buffer, size_t buffer_size,
                                           size_t* buffer_offset, const uint8_t** uart_src,
                                           const uint8_t* uart_end,
                                           PacketLengthFunction get_packet_length,
                                           zx::channel* channel, bt_hci_snoop_type_t snoop_type);

  void OnChannelSignal(Wait* wait, zx_status_t status, const zx_packet_signal_t* signal);

  zx_status_t HciOpenChannel(zx::channel* in_channel, zx_handle_t in);

  // 1 byte packet indicator + 3 byte header + payload
  static constexpr uint32_t kCmdBufSize = 255 + 4;

  // The maximum HCI ACL frame size used for data transactions
  // (1024 + 4 bytes for the ACL header + 1 byte packet indicator)
  static constexpr uint32_t kAclMaxFrameSize = 1029;

  // The maximum HCI SCO frame size used for data transactions.
  // (255 byte payload + 3 bytes for the SCO header + 1 byte packet indicator)
  static constexpr uint32_t kScoMaxFrameSize = 259;

  // 1 byte packet indicator + 2 byte header + payload
  static constexpr uint32_t kEventBufSize = 255 + 3;

  // Backing channel for this device.
  zx::channel in_channel_;
  Wait in_channel_wait_{this, &in_channel_};

  // Upper channels.
  zx::channel cmd_channel_;
  Wait cmd_channel_wait_{this, &cmd_channel_};

  zx::channel acl_channel_;
  Wait acl_channel_wait_{this, &acl_channel_};

  zx::channel sco_channel_;
  Wait sco_channel_wait_{this, &sco_channel_};

  zx::channel snoop_channel_;

  std::atomic_bool shutting_down_ = false;

  // Type of current packet being read from the UART.
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  BtHciPacketIndicator cur_uart_packet_type_ = kHciNone;

  // For accumulating HCI events.
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  uint8_t event_buffer_[kEventBufSize];
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  size_t event_buffer_offset_ = 0;

  // For accumulating ACL data packets.
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  uint8_t acl_buffer_[kAclMaxFrameSize];
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  size_t acl_buffer_offset_ = 0;

  // For accumulating SCO packets.
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  uint8_t sco_buffer_[kScoMaxFrameSize];
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  size_t sco_buffer_offset_ = 0;

  // For sending outbound packets to the UART.
  // |kAclMaxFrameSize| is the largest frame size sent.
  uint8_t write_buffer_[kAclMaxFrameSize];

  driver_devfs::Connector<fuchsia_hardware_bluetooth::Vendor> devfs_connector_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Vendor> vendor_binding_group_;

  async_dispatcher_t* dispatcher_ = fdf::Dispatcher::GetCurrent()->async_dispatcher();
};

}  // namespace bt_hci_virtual

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_LOOPBACK_H_
