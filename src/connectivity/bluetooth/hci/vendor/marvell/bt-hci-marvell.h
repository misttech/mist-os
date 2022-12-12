// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_BT_HCI_MARVELL_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_BT_HCI_MARVELL_H_

#include <fuchsia/hardware/bt/hci/cpp/banjo.h>
#include <fuchsia/hardware/sdio/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <ddktl/device.h>

#include "src/connectivity/bluetooth/hci/vendor/marvell/device-oracle.h"

namespace bt_hci_marvell {

// How many seconds to wait for the firmware to be loaded by another (wlan) driver before failing
// our ddkInit operation.
constexpr size_t kFirmwareWaitSeconds = 300;

class BtHciMarvell;
using BtHciMarvellType =
    ddk::Device<BtHciMarvell, ddk::GetProtocolable, ddk::Initializable, ddk::Unbindable>;

class BtHciMarvell : public BtHciMarvellType, public ddk::BtHciProtocol<BtHciMarvell> {
 public:
  BtHciMarvell(zx_device_t* parent, const ddk::SdioProtocolClient& sdio)
      : BtHciMarvellType(parent), sdio_(sdio) {}
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

  // Report initialization status to the driver manager
  void OnInitComplete(zx_status_t status, ddk::InitTxn txn);

  // Load firmware (or wait for notification that firmware has been loaded by another driver)
  zx_status_t LoadFirmware();

  // Bytewide SDIO operations
  zx_status_t Read8(uint32_t addr, uint8_t* out_value);
  zx_status_t Read16(uint32_t addr, uint16_t* out_value);
  zx_status_t Read24(uint32_t addr, uint32_t* out_value);
  zx_status_t Write8(uint32_t addr, uint8_t value);

  const ddk::SdioProtocolClient sdio_;

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
