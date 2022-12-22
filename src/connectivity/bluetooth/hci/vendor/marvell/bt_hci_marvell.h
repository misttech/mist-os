// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_BT_HCI_MARVELL_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_BT_HCI_MARVELL_H_

#include <fuchsia/hardware/bt/hci/cpp/banjo.h>
#include <fuchsia/hardware/sdio/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <ddktl/device.h>
#include <fbl/mutex.h>

#include "src/connectivity/bluetooth/hci/vendor/marvell/device_oracle.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/host_channel_manager.h"

namespace bt_hci_marvell {

// How many seconds to wait for the firmware to be loaded by another (wlan) driver before failing
// our ddkInit operation.
constexpr size_t kFirmwareWaitSeconds = 300;

class BtHciMarvell;
using BtHciMarvellType =
    ddk::Device<BtHciMarvell, ddk::GetProtocolable, ddk::Initializable, ddk::Unbindable>;

class BtHciMarvell : public BtHciMarvellType, public ddk::BtHciProtocol<BtHciMarvell> {
 public:
  BtHciMarvell(zx_device_t* parent, const ddk::SdioProtocolClient& sdio, zx::port port)
      : BtHciMarvellType(parent),
        sdio_(sdio),
        sdio_interrupt_key_(interrupt_key_mgr_.CreateKey()),
        stop_thread_key_(interrupt_key_mgr_.CreateKey()),
        port_(std::move(port)) {}
  virtual ~BtHciMarvell() = default;

  // Allocate a new instance of the driver and register with the driver manager
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  // ddk::Device
  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out_proto);

  // ddk::BtHciProtocol
  zx_status_t BtHciOpenCommandChannel(zx::channel channel);
  zx_status_t BtHciOpenAclDataChannel(zx::channel channel);
  zx_status_t BtHciOpenScoChannel(zx::channel channel);
  zx_status_t BtHciOpenSnoopChannel(zx::channel channel);
  void BtHciConfigureSco(sco_coding_format_t coding_format, sco_encoding_t encoding,
                         sco_sample_rate_t sample_rate, bt_hci_configure_sco_callback callback,
                         void* cookie);
  void BtHciResetSco(bt_hci_reset_sco_callback callback, void* cookie);

 private:
  // Initialize hardware registers and load firmware as needed
  zx_status_t Init();

  // Load firmware (or wait for notification that firmware has been loaded by another driver)
  zx_status_t LoadFirmware();

  zx_status_t EnableHostInterrupts();
  zx_status_t DisableHostInterrupts();

  // Bytewide SDIO operations
  zx_status_t Read8(uint32_t addr, uint8_t* out_value);
  zx_status_t Read16(uint32_t addr, uint16_t* out_value);
  zx_status_t Read24(uint32_t addr, uint32_t* out_value);
  zx_status_t Write8(uint32_t addr, uint8_t value);

  // Read from register |addr|, modify the bits corresponding to the locations of bits set in |mask|
  // with bits in the same location from |value|, and then write back out to the register.
  zx_status_t ModifyBits(uint32_t addr, uint8_t mask, uint8_t value);

  // Create a HostChannel object that will use |in_channel| to communicate with the host. |read_id|
  // is the channel id that will be used in the controller header when we read from this host
  // channel. |write_id| is the id that the controller will use in the packet header when it is
  // giving us a packet that should be written to this channel. |write_id| must be unique across
  // all open channels.
  zx_status_t OpenChannel(zx::channel in_channel, ControllerChannelId read_id,
                          ControllerChannelId write_id, const char* name);

  const ddk::SdioProtocolClient sdio_;

  // Initialized once (during Init()) and then never written to again.
  zx::interrupt sdio_int_;

  fbl::Mutex mutex_;

  // Tracks all currently-allocated interrupt keys
  InterruptKeyAllocator interrupt_key_mgr_ TA_GUARDED(mutex_);

  // Keys for event handler messages
  const uint64_t sdio_interrupt_key_;
  const uint64_t stop_thread_key_;

  // Keeps track of all open communication channels with the host
  HostChannelManager channel_mgr_ TA_GUARDED(mutex_);

  // All events (from the controller and from the host) will be sent through this port, which will
  // wake up our event handler loop.
  zx::port port_;

  // The oracle of all values that are device-specific
  std::unique_ptr<DeviceOracle> device_oracle_;

  // The async loop (and corresponding dispatcher) on which all our driver tasks will run. The
  // dispatcher is initialized during the call to DdkInit().
  std::optional<async::Loop> loop_;
  async_dispatcher_t* dispatcher_;

  // The address where we will exchange data frames with the target
  uint32_t ioport_addr_ = 0xffffffff;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_BT_HCI_MARVELL_H_
