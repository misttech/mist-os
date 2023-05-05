// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/bt_hci_marvell.h"

#include <endian.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/fzl/vmo-mapper.h>
#include <sys/random.h>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/connectivity/bluetooth/hci/vendor/marvell/marvell_frame.emb.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/marvell_hci.h"

#include <pw_bluetooth/hci.emb.h>

namespace bt_hci_marvell {

zx_status_t BtHciMarvell::Bind(void* ctx, zx_device_t* parent) {
  zx_status_t status;
  ddk::SdioProtocolClient sdio(parent);

  if (!sdio.is_valid()) {
    zxlogf(ERROR, "Failed to get SDIO protocol");
    return ZX_ERR_NO_RESOURCES;
  }

  zx::port port;
  status = zx::port::create(ZX_PORT_BIND_TO_INTERRUPT, &port);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Failed to create port", __FILE__);
    return status;
  }

  // Allocate our driver instance
  fbl::AllocChecker ac;
  std::unique_ptr<BtHciMarvell> device(new (&ac) BtHciMarvell(parent, sdio, std::move(port)));
  if (!ac.check()) {
    zxlogf(ERROR, "BtHciMarvell alloc failed");
    return ZX_ERR_NO_MEMORY;
  }

  // Add our device to the device tree
  ddk::DeviceAddArgs args("bt-hci-marvell");
  args.set_proto_id(ZX_PROTOCOL_BT_HCI);
  if ((status = device->DdkAdd(args)) != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %s", zx_status_get_string(status));
    return status;
  }

  // Driver Manager now owns the device - memory will be explicitly freed in DdkRelease()
  device.release();

  return ZX_OK;
}

void BtHciMarvell::DdkInit(ddk::InitTxn txn) {
  init_txn_ = std::make_unique<ddk::InitTxn>(std::move(txn));

  // Start up an independent driver thread
  int status = thrd_create_with_name(
      &driver_thread_, [](void* ctx) { return reinterpret_cast<BtHciMarvell*>(ctx)->Thread(); },
      this, "bt-hci-marvell-thread");
  if (status != thrd_success) {
    zxlogf(ERROR, "Failed to start thread: %s", zx_status_get_string(status));
    init_txn_->Reply(status);
    init_txn_.reset();
    return;
  }
}

int BtHciMarvell::Thread() {
  zx_status_t status = Init();
  if (status == ZX_OK) {
    EventHandler();
  }
  if (init_txn_) {
    init_txn_->Reply(status);
    init_txn_.reset();
  }
  return thrd_success;
}

zx_status_t BtHciMarvell::Init() {
  zx_status_t status;
  sdio_hw_info_t hw_info;

  // Retrieve our product ID
  if ((status = sdio_.GetDevHwInfo(&hw_info)) != ZX_OK) {
    zxlogf(ERROR, "Failed to find SDIO hardware info: %s", zx_status_get_string(status));
    return status;
  }
  uint32_t product_id = hw_info.func_hw_info.product_id;

  // Determine if this device is supported. If so, instantiate our oracle.
  zx::result<DeviceOracle> result = DeviceOracle::Create(product_id);
  if (result.is_error()) {
    zxlogf(ERROR, "Unable to find supported device matching product id %0x" PRIu32, product_id);
    return result.error_value();
  }
  device_oracle_ = result.value();

  if ((status = sdio_.GetInBandIntr(&sdio_int_)) != ZX_OK) {
    zxlogf(ERROR, "Failed to get SDIO interrupt: %s", zx_status_get_string(status));
    return status;
  }

  // Associate SDIO interrupts with the port so that interrupts will post events to the port's queue
  if ((status = sdio_int_.bind(port_, sdio_interrupt_key_, 0)) != ZX_OK) {
    zxlogf(ERROR, "Failed to bind interrupt to port: %s", zx_status_get_string(status));
    return status;
  }

  if ((status = sdio_.EnableFn()) != ZX_OK) {
    zxlogf(ERROR, "Failed to enable SDIO function: %s", zx_status_get_string(status));
    return status;
  }

  // Set the SDIO block size
  uint16_t block_size = device_oracle_->GetSdioBlockSize();
  if ((status = sdio_.UpdateBlockSize(block_size, false)) != ZX_OK) {
    zxlogf(ERROR, "Failed to set SDIO block size to %u: %s", block_size,
           zx_status_get_string(status));
    return status;
  }

  if ((status = LoadFirmware()) != ZX_OK) {
    zxlogf(ERROR, "Failed to load firmware: %s", zx_status_get_string(status));
    return status;
  }

  // Retrieve the address of the IO Port, which is where we will perform read/writes of HCI packets
  uint32_t ioport_addr_reg_addr = device_oracle_->GetRegAddrIoportAddr();
  if ((status = Read24(ioport_addr_reg_addr, &ioport_addr_)) != ZX_OK) {
    return status;
  }
  zxlogf(INFO, "IO port address: %#x", ioport_addr_);

  // Set interrupt behavior to "clear-on-read"
  uint32_t rsr_addr = device_oracle_->GetRegAddrInterruptRsr();
  if ((status = ModifyBits(rsr_addr, kRsrClearOnReadMask, kRsrClearOnReadValue)) != ZX_OK) {
    return status;
  }

  // Configure interrupts to automatically re-enable
  uint32_t misc_cfg_reg_addr = device_oracle_->GetRegAddrMiscCfg();
  if ((status = ModifyBits(misc_cfg_reg_addr, kMiscCfgAutoReenableMask,
                           kMiscCfgAutoReenableValue)) != ZX_OK) {
    return status;
  }

  if ((status = EnableHostInterrupts()) != ZX_OK) {
    zxlogf(ERROR, "Failed to enable host interrupts: %s", zx_status_get_string(status));
    return status;
  }

  if ((status = OpenVendorCmdChannel()) != ZX_OK) {
    return status;
  }

  if ((status = SetMacAddr()) != ZX_OK) {
    zxlogf(ERROR, "Failed to set mac address: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

// For now, we rely on wlan to load the firmware image for us, we just have to wait for it.
zx_status_t BtHciMarvell::LoadFirmware() {
  uint32_t fw_status_reg_addr = device_oracle_->GetRegAddrFirmwareStatus();

  for (size_t secs_left = 0; secs_left < kFirmwareWaitSeconds; secs_left++) {
    zx_status_t status;
    uint16_t fw_status = 0xffff;

    status = Read16(fw_status_reg_addr, &fw_status);
    if (status == ZX_OK && fw_status == kFirmwareStatusReady) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(zx::sec(1)));
  }
  return ZX_ERR_TIMED_OUT;
}

zx_status_t BtHciMarvell::OpenVendorCmdChannel() {
  zx_status_t status;
  zx::channel channel_mgr_handle;

  status = zx::channel::create(0, &channel_mgr_handle, &vendor_cmd_channel_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to open vendor command channel: %s", zx_status_get_string(status));
    return status;
  }

  return OpenChannel(std::move(channel_mgr_handle), ControllerChannelId::kChannelVendor,
                     ControllerChannelId::kChannelVendor, "Vendor Command");
}

zx_status_t BtHciMarvell::SetMacAddr() {
  constexpr size_t kMacAddrLen = 6;
  zx_status_t status;
  uint8_t mac_addr[kMacAddrLen];
  size_t actual_len;

  // Read the MAC address from boot data
  status = DdkGetMetadata(DEVICE_METADATA_MAC_ADDRESS, mac_addr, sizeof(mac_addr), &actual_len);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Bootloader failed to find BT mac address: %s", zx_status_get_string(status));
    return status;
  }
  if (actual_len < kMacAddrLen) {
    zxlogf(ERROR,
           "Bootloader returned incomplete mac address (%zu bytes received, expected of %zu)",
           actual_len, sizeof(mac_addr));
  }
  zxlogf(INFO, "Using BT mac address: %02x:%02x:%02x:%02x:%02x:%02x", mac_addr[0], mac_addr[1],
         mac_addr[2], mac_addr[3], mac_addr[4], mac_addr[5]);

  constexpr size_t kCommandHeaderSize = pw::bluetooth::emboss::CommandHeaderView::SizeInBytes();
  constexpr size_t kVendorCommandSize = MarvellSetMacAddrCommandView::SizeInBytes();
  uint8_t frame[kCommandHeaderSize + kVendorCommandSize];

  // Write out generic HCI command header
  auto command_header_view =
      pw::bluetooth::emboss::MakeCommandHeaderView(frame, kCommandHeaderSize);
  command_header_view.opcode().ogf().Write(kHciOgfVendorSpecificDebug);
  command_header_view.opcode().ocf().Write(kHciOcfMarvellSetMacAddr);
  command_header_view.parameter_total_size().Write(static_cast<uint8_t>(kVendorCommandSize));

  // Write out Marvell-specific SetMacAddr command header
  auto vendor_cmd_view =
      MakeMarvellSetMacAddrCommandView(&frame[kCommandHeaderSize], kVendorCommandSize);
  vendor_cmd_view.marvell_header().cmd_type().Write(
      static_cast<uint8_t>(ControllerChannelId::kChannelVendor));
  vendor_cmd_view.marvell_header().cmd_length().Write(static_cast<uint8_t>(kMacAddrLen));

  // And finally, the SetMacAddr command data, which needs to be reversed
  uint8_t* dst_mac_addr = vendor_cmd_view.mac().BackingStorage().data();
  for (size_t ndx = 0; ndx < kMacAddrLen; ndx++) {
    dst_mac_addr[ndx] = mac_addr[(kMacAddrLen - 1) - ndx];
  }

  // Write the command to the vendor command channel, which will be processed by the event loop
  if ((status = vendor_cmd_channel_.write(0, &frame, sizeof(frame), nullptr, 0)) != ZX_OK) {
    zxlogf(ERROR, "Failed to write to vendor command channel: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

bool BtHciMarvell::ProcessVendorEvent(const uint8_t* frame, size_t frame_size) {
  bool is_handled = false;

  // At the moment, the only vendor-specific event we handle is a SetMacAddr, which is the last
  // step in initialization. So we don't need to run these checks unless we're waiting on it.
  if (!init_txn_) {
    return is_handled;
  }

  // Verify that the frame was sent to the "Event" channel
  const auto frame_view = MakeMarvellFrameView(frame, frame_size);
  if (frame_view.header().channel_id().Read() !=
      static_cast<uint8_t>(ControllerChannelId::kChannelEvent)) {
    return is_handled;
  }

  // Verify the HCI event header
  const size_t payload_size = frame_view.header().payload_size().Read();
  const uint8_t* payload_data = frame_view.payload().BackingStorage().data();
  const auto event_view = pw::bluetooth::emboss::MakeEventHeaderView(payload_data, payload_size);
  if (!event_view.Ok()) {
    return is_handled;
  }
  if (event_view.event_code().Read() != kHciEventCodeCommandComplete) {
    return is_handled;
  }

  // Verify the HCI Command Complete event fields
  const auto cmd_complete_event_view =
      pw::bluetooth::emboss::MakeCommandCompleteEventView(payload_data, payload_size);
  uint16_t opcode = cmd_complete_event_view.command_opcode().BackingStorage().ReadUInt();
  if (opcode != kHciOpcodeMarvellSetMacAddr) {
    return is_handled;
  }

  // Beyond this point, we know it's a Marvell SetMacAddr operation, so we will handle all necessary
  // processing ourselves and won't expect the frame to be passed back to the host.
  is_handled = true;

  // Isolate the return parameters and cross-check sizes
  const size_t return_params_length = cmd_complete_event_view.return_parameters_size().Read();
  if (return_params_length < 1) {
    zxlogf(ERROR,
           "Insufficient parameters received in response to vendor SetMacAddr HCI operation");
    return is_handled;
  }
  if (payload_size <
      (pw::bluetooth::emboss::CommandCompleteEventView::SizeInBytes() + return_params_length)) {
    zxlogf(ERROR, "Improperly formatted command complete frame - ignoring");
    return is_handled;
  }
  const uint8_t* return_params =
      &payload_data[pw::bluetooth::emboss::CommandCompleteEventView::SizeInBytes()];

  // Finally, check that the status code returned is correct
  if (return_params[0] != kHciMarvellCmdCompleteSuccess) {
    zxlogf(ERROR, "Attempt to set mac address rejected by controller with status %#02x",
           return_params[0]);
    return is_handled;
  }

  zxlogf(INFO, "Received confirmation of mac address set - sending ddkInit response");
  init_txn_->Reply(ZX_OK);
  init_txn_.reset();
  return is_handled;
}

void BtHciMarvell::EventHandler() {
  zx_status_t status;
  zx_port_packet_t packet;
  bool halt = false;

  do {
    // If we haven't completed initialization, then we are still waiting to get a response from the
    // controller to our set mac address request. In that case, we will limit our wait time to a
    // finite value.
    zx::duration wait_duration =
        init_txn_ ? zx::sec(kVendorCmdResponseWaitSeconds) : zx::duration::infinite();
    status = port_.wait(zx::deadline_after(wait_duration), &packet);

    if (status != ZX_OK) {
      zxlogf(ERROR, "Port wait failed (%s), terminating event handler",
             zx_status_get_string(status));
      break;
    }

    switch (packet.type) {
      case ZX_PKT_TYPE_INTERRUPT:
        // We received an interrupt from the controller
        if (packet.key == sdio_interrupt_key_) {
          ProcessSdioInterrupt();
        } else {
          zxlogf(ERROR, "Interrupt received from unrecognized source (%" PRIu64 ") - ignoring",
                 packet.key);
        }
        break;

      case ZX_PKT_TYPE_SIGNAL_ONE:
        // We received an event from one of our channels
        ProcessChannelEvent(packet);
        break;

      case ZX_PKT_TYPE_USER:
        // We received a notification to shut down the event handler
        if (packet.key == stop_thread_key_) {
          halt = true;
        } else {
          zxlogf(ERROR, "User packet received from unrecognized source (%" PRIu64 ") - ignoring",
                 packet.key);
        }
        break;

      default:
        zxlogf(ERROR, "Unrecognized port notificaction type (%u) - ignoring", packet.type);
        break;
    }
  } while (!halt);
}

zx_status_t BtHciMarvell::ProcessSdioInterrupt() {
  zx_status_t status;
  uint8_t interrupt_status;

  fbl::AutoLock lock(&mutex_);

  // Reading from the Interrupt Status register will re-arm the interrupt on the card
  status = Read8(device_oracle_->GetRegAddrInterruptStatus(), &interrupt_status);
  if ((status == ZX_OK) && (interrupt_status != 0)) {
    // Card has a frame ready for us to read
    if (interrupt_status & kInterruptMaskPacketAvailable) {
      ReadFromCard();
    }
    // Card is ready to process another frame
    if (interrupt_status & kInterruptMaskReadyToSend) {
      // Checking for tx_allowed_ makes sure we don't accidentally have multiple async_waits active
      // on a single channel.
      if (!tx_allowed_) {
        tx_allowed_ = true;
        channel_mgr_.ForEveryChannel(
            [this](const HostChannel* host_channel) { ArmChannelInterrupts(host_channel); });
      } else {
        zxlogf(WARNING, "Controller has reported redundant tx confirmations");
      }
    }
  }

  // re-arm the SDIO interrupt
  sdio_int_.ack();
  sdio_.AckInBandIntr();

  return status;
}

zx_status_t BtHciMarvell::ReadFromCard() {
  zx_status_t status;

  // Calculate the length of our read -- this tells us how big to allocate our buffer, but is not
  // a precise frame length (which we retrieve from the actual header after we've completed the
  // read). The read length is given to us in the rx_len and rx_unit registers as:
  // (rx_len << rx_unit) + header size.
  uint32_t rx_len_reg_addr = device_oracle_->GetRegAddrRxLen();
  uint8_t rx_len;
  if ((status = Read8(rx_len_reg_addr, &rx_len)) != ZX_OK) {
    return status;
  }
  uint32_t rx_unit_reg_addr = device_oracle_->GetRegAddrRxUnit();
  uint8_t rx_unit;
  if ((status = Read8(rx_unit_reg_addr, &rx_unit)) != ZX_OK) {
    return status;
  }
  if (rx_unit > 31) {
    zxlogf(ERROR, "Invalid value read from rx_unit register: %d - ignoring", rx_unit);
    return ZX_ERR_IO;
  }
  uint32_t packet_size = static_cast<uint32_t>(rx_len) << rx_unit;
  if (packet_size > kMarvellMaxRxFrameSize) {
    zxlogf(ERROR,
           "Packet size (%" PRIu32
           ") exceeds maximum rx frame size for this device - "
           "ignoring",
           packet_size);
    return ZX_ERR_IO;
  }
  uint32_t buffer_size =
      fbl::round_up<uint32_t, uint32_t>(packet_size, device_oracle_->GetSdioBlockSize());

  // Allocate the buffer and create some useful pointers into it
  fzl::VmoMapper mapper;
  zx::vmo vmo;
  status = mapper.CreateAndMap(buffer_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                               /* vmar_manager */ nullptr, &vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create and map VMO: %s", zx_status_get_string(status));
    return status;
  }
  const uint8_t* buffer_ptr = reinterpret_cast<uint8_t*>(mapper.start());
  const auto frame_view = MakeMarvellFrameView(buffer_ptr, buffer_size);
  if (!frame_view.IsComplete()) {
    zxlogf(ERROR, "Failure parsing incoming frame - ignoring");
    return ZX_ERR_IO_DATA_INTEGRITY;
  }

  // Define the buffer region
  sdmmc_buffer_region_t buffer;
  buffer.buffer.vmo = vmo.get();
  buffer.type = SDMMC_BUFFER_TYPE_VMO_HANDLE;
  buffer.offset = 0;
  buffer.size = buffer_size;

  // Define the SDIO transaction using the buffer region
  sdio_rw_txn txn;
  txn.addr = ioport_addr_;
  txn.incr = false;
  txn.write = false;
  txn.buffers_list = &buffer;
  txn.buffers_count = 1;

  // Finally, send to the SDIO controller for execution
  if ((status = sdio_.DoRwTxn(&txn)) != ZX_OK) {
    zxlogf(ERROR, "SDIO transaction failed: %s", zx_status_get_string(status));
    return status;
  }

  if (ProcessVendorEvent(buffer_ptr, buffer_size)) {
    return ZX_OK;
  }

  // Write the frame to the appropriate channel based on its channel Id. Header size includes the
  // header itself.
  uint8_t raw_channel_id = frame_view.header().channel_id().Read();
  ControllerChannelId target_channel_id = static_cast<ControllerChannelId>(raw_channel_id);
  const HostChannel* host_channel = channel_mgr_.HostChannelFromWriteId(target_channel_id);
  if (!host_channel) {
    zxlogf(ERROR, "Packet received (id: %#x), but no matching channel open - dropping",
           raw_channel_id);
    return ZX_ERR_UNAVAILABLE;
  }
  uint32_t write_size = frame_view.header().payload_size().Read();
  const uint8_t* payload = frame_view.payload().BackingStorage().data();
  if ((status = host_channel->channel().write(/* options */ 0, payload, write_size,
                                              /* handles */ nullptr, /* num_handles */ 0)) !=
      ZX_OK) {
    zxlogf(ERROR, "Failed to write to channel %s: %s", host_channel->name(),
           zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

void BtHciMarvell::ProcessChannelEvent(const zx_port_packet_t& packet) {
  fbl::AutoLock lock(&mutex_);
  const HostChannel* host_channel = channel_mgr_.HostChannelFromInterruptKey(packet.key);
  if (host_channel == nullptr) {
    zxlogf(ERROR, "Can't find channel associated with interrupt %" PRIu64, packet.key);
    return;
  }

  // Data available, pass it along to the controller
  if (packet.signal.observed & ZX_CHANNEL_READABLE) {
    if (WriteToCard(host_channel) == ZX_OK) {
      // Disable interrupts while we wait for the controller to tell us its ready for the next
      // frame.
      tx_allowed_ = false;
      channel_mgr_.ForEveryChannel([this, host_channel](const HostChannel* ch) {
        if (ch != host_channel) {
          DisarmChannelInterrupts(ch);
        }
      });
    }
  }

  // Channel shut down
  if (packet.signal.observed & (ZX_CHANNEL_PEER_CLOSED | ZX_SIGNAL_HANDLE_CLOSED)) {
    zxlogf(INFO, "Shutting down %s channel", host_channel->name());
    interrupt_key_mgr_.RemoveKey(host_channel->interrupt_key());
    channel_mgr_.RemoveChannel(host_channel->write_id());
    return;
  }

  // If the channel is still open and the controller can accept new frames, we need to reset the
  // wait_async for this channel. This should only ever happen if we have a failure while trying
  // to write out the packet to the SDIO bus.
  if (tx_allowed_) {
    ArmChannelInterrupts(host_channel);
  }
}

zx_status_t BtHciMarvell::WriteToCard(const HostChannel* host_channel) {
  constexpr size_t kMarvellFrameHeaderSize = MarvellFrameHeaderView::SizeInBytes();

  // Query the channel to determine the size of our buffer
  uint32_t size_without_header;
  zx_status_t status = host_channel->channel().read(/* options */ 0,
                                                    /* bytes */ nullptr,
                                                    /* handles */ nullptr,
                                                    /* num_bytes */ 0,
                                                    /* num_handles */ 0, &size_without_header,
                                                    /* actual_handles */ nullptr);
  if (status != ZX_ERR_BUFFER_TOO_SMALL) {
    zxlogf(ERROR, "Failed to read data size from %s channel", host_channel->name());
    return status;
  }
  uint32_t size_with_header = size_without_header + kMarvellFrameHeaderSize;

  // Allocate a vmo to hold the data from the channel
  uint32_t total_buffer_size =
      fbl::round_up<uint32_t, uint32_t>(size_with_header, device_oracle_->GetSdioBlockSize());
  fzl::VmoMapper mapper;
  zx::vmo vmo;
  status = mapper.CreateAndMap(total_buffer_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                               /* vmar_manager */ nullptr, &vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create and map VMO for %s channel frame", host_channel->name());
    return status;
  }

  // Read the data from the channel
  const uint32_t frame_buf_size = static_cast<uint32_t>(mapper.size() - kMarvellFrameHeaderSize);
  uint8_t* frame_buf = reinterpret_cast<uint8_t*>(mapper.start());
  uint8_t* payload_buf = frame_buf + kMarvellFrameHeaderSize;
  uint32_t actual_bytes_read;
  status = host_channel->channel().read(/* options */ 0, payload_buf,
                                        /* handles */ nullptr, frame_buf_size,
                                        /* num_handles */ 0, &actual_bytes_read,
                                        /* actual_handles */ nullptr);
  if (status != ZX_OK) {
    zxlogf(WARNING, "Failed to read from %s channel: %s", host_channel->name(),
           zx_status_get_string(status));
    return status;
  }

  // Fill in the header fields - size includes the header itself
  if (size_without_header != actual_bytes_read) {
    zxlogf(WARNING, "Inconsistent frame size in consecutive reads (%" PRIu32 " vs. %" PRIu32 ")",
           size_without_header, actual_bytes_read);
    size_without_header = actual_bytes_read;
    size_with_header = size_without_header + kMarvellFrameHeaderSize;
  }
  auto frame_view = MakeMarvellFrameView(frame_buf, kMarvellFrameHeaderSize);
  frame_view.header().total_frame_size().Write(size_with_header);
  frame_view.header().channel_id().Write(static_cast<uint8_t>(host_channel->read_id()));

  memset(&frame_buf[size_with_header], kUnusedSdioByteFiller, total_buffer_size - size_with_header);

  // And write out to the controller
  return WriteToIoport(total_buffer_size, vmo);
}

zx_status_t BtHciMarvell::Read8(uint32_t addr, uint8_t* out_value) {
  zx_status_t status = sdio_.DoRwByte(/* write */ false, addr, /* write_byte */ 0, out_value);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failure reading from address %#x: %s", addr, zx_status_get_string(status));
    *out_value = 0xff;
  }
  return status;
}

// Read two adjacent (but not necessarily aligned) registers, and return the value adjusted for
// host endianness.
zx_status_t BtHciMarvell::Read16(uint32_t addr, uint16_t* out_value) {
  zx_status_t status;
  *out_value = 0xffff;
  uint8_t lobits, hibits;

  // Block reads have to be word-aligned, so it's a bit more straightforward to just read the
  // bytes independently.
  if (((status = Read8(addr, &lobits)) == ZX_OK) &&
      ((status = Read8(addr + 1, &hibits)) == ZX_OK)) {
    *out_value = hibits;
    (*out_value) <<= CHAR_BIT;
    *out_value |= lobits;
  }
  return status;
}

// Read three adjacent (but not necessarily aligned) registers, and return the value adjusted for
// host endianness.
zx_status_t BtHciMarvell::Read24(uint32_t addr, uint32_t* out_value) {
  zx_status_t status;
  *out_value = 0xffff;
  uint8_t lobits, midbits, hibits;

  if (((status = Read8(addr, &lobits)) == ZX_OK) &&
      ((status = Read8(addr + 1, &midbits)) == ZX_OK) &&
      ((status = Read8(addr + 2, &hibits)) == ZX_OK)) {
    *out_value = hibits;
    (*out_value) <<= CHAR_BIT;
    *out_value |= midbits;
    (*out_value) <<= CHAR_BIT;
    *out_value |= lobits;
  }
  return status;
}

zx_status_t BtHciMarvell::Write8(uint32_t addr, uint8_t value) {
  zx_status_t status = sdio_.DoRwByte(/* write */ true, addr, value, /* out_read_byte */ nullptr);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failure writing to address %#x: %s", addr, zx_status_get_string(status));
  }
  return status;
}

zx_status_t BtHciMarvell::ModifyBits(uint32_t addr, uint8_t mask, uint8_t new_value) {
  zx_status_t status;
  uint8_t reg_contents = 0xff;
  if ((status = Read8(addr, &reg_contents)) != ZX_OK) {
    return status;
  }
  reg_contents &= ~mask;
  reg_contents |= (new_value & mask);
  return Write8(addr, reg_contents);
}

zx_status_t BtHciMarvell::WriteToIoport(uint32_t size, const zx::vmo& vmo) {
  zx_status_t status;

  // Create a single buffer corresponding to our data
  sdmmc_buffer_region_t buffer;
  buffer.buffer.vmo = vmo.get();
  buffer.type = SDMMC_BUFFER_TYPE_VMO_HANDLE;
  buffer.offset = 0;
  ZX_ASSERT(size % device_oracle_->GetSdioBlockSize() == 0);
  buffer.size = size;

  // Create a transaction containing only the buffer
  sdio_rw_txn txn;
  txn.addr = ioport_addr_;
  txn.incr = false;
  txn.write = true;
  txn.buffers_list = &buffer;
  txn.buffers_count = 1;

  // And execute that transaction
  if ((status = sdio_.DoRwTxn(&txn)) != ZX_OK) {
    zxlogf(ERROR, "SDIO write transaction failed: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t BtHciMarvell::EnableHostInterrupts() {
  // Enable on the target controller
  zx_status_t status = ModifyBits(device_oracle_->GetRegAddrInterruptMask(), kInterruptMaskAllBits,
                                  kInterruptMaskReadyToSend | kInterruptMaskPacketAvailable);
  if (status != ZX_OK) {
    // Failure diagnostics are provided by the lower-level SDIO read/write functions
    return status;
  }

  // Enable on the host (ourselves)
  if ((status = sdio_.EnableFnIntr()) != ZX_OK) {
    zxlogf(ERROR, "Failed to enable function interrupt: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

void BtHciMarvell::DdkUnbind(ddk::UnbindTxn txn) { txn.Reply(); }

void BtHciMarvell::TerminateEventHandler() {
  // Send a packet to port_ to wake up the thread
  zx_port_packet_t packet;
  packet.key = stop_thread_key_;
  packet.type = ZX_PKT_TYPE_USER;
  if (port_.queue(&packet) != ZX_OK) {
    zxlogf(ERROR, "%s: Failed to queue stop_thread packet\n", __FILE__);
  }
}

void BtHciMarvell::DdkRelease() {
  TerminateEventHandler();
  thrd_join(driver_thread_, nullptr);
  delete this;
}

zx_status_t BtHciMarvell::DdkGetProtocol(uint32_t proto_id, void* out_proto) {
  if (proto_id == ZX_PROTOCOL_BT_HCI) {
    bt_hci_protocol_t* hci_proto = static_cast<bt_hci_protocol_t*>(out_proto);
    hci_proto->ops = &bt_hci_protocol_ops_;
    hci_proto->ctx = this;
    return ZX_OK;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t BtHciMarvell::ArmChannelInterrupts(const HostChannel* host_channel) {
  zx_status_t status;
  zx_signals_t wait_signals =
      ZX_CHANNEL_PEER_CLOSED | ZX_SIGNAL_HANDLE_CLOSED | ZX_CHANNEL_READABLE;
  if ((status = host_channel->channel().wait_async(port_, host_channel->interrupt_key(),
                                                   wait_signals, 0)) != ZX_OK) {
    zxlogf(ERROR, "Failed to configure wait on %s channel", host_channel->name());
  }
  return status;
}

zx_status_t BtHciMarvell::DisarmChannelInterrupts(const HostChannel* host_channel) {
  zx_status_t status;
  if ((status = port_.cancel(host_channel->channel(), host_channel->interrupt_key())) != ZX_OK) {
    zxlogf(ERROR, "Failed to cancel async_wait on %s channel: %s", host_channel->name(),
           zx_status_get_string(status));
  }
  return status;
}

zx_status_t BtHciMarvell::OpenChannel(zx::channel in_channel, ControllerChannelId read_id,
                                      ControllerChannelId write_id, const char* name) {
  fbl::AutoLock lock(&mutex_);

  uint64_t port_key = interrupt_key_mgr_.CreateKey();
  const HostChannel* new_channel_ref =
      channel_mgr_.AddChannel(std::move(in_channel), read_id, write_id, port_key, name);
  if (!new_channel_ref) {
    zxlogf(ERROR, "Failed to open %s channel", name);
    interrupt_key_mgr_.RemoveKey(port_key);
    return ZX_ERR_INTERNAL;
  }

  if (tx_allowed_) {
    ArmChannelInterrupts(new_channel_ref);
  }

  return ZX_OK;
}

zx_status_t BtHciMarvell::BtHciOpenCommandChannel(zx::channel channel) {
  // Commands are passed host->controller and events are passed controller->host
  return OpenChannel(std::move(channel), ControllerChannelId::kChannelCommand,
                     ControllerChannelId::kChannelEvent, "Command");
}

zx_status_t BtHciMarvell::BtHciOpenAclDataChannel(zx::channel channel) {
  // The same ID is used for ACL data regardless of whether it is going from host->controller,
  // or controller->host.
  return OpenChannel(std::move(channel), ControllerChannelId::kChannelAclData,
                     ControllerChannelId::kChannelAclData, "ACL Data");
}

zx_status_t BtHciMarvell::BtHciOpenScoChannel(zx::channel channel) {
  // The same ID is used for SCO data regardless of whether it is going from host->controller,
  // or controller->host.
  return OpenChannel(std::move(channel), ControllerChannelId::kChannelScoData,
                     ControllerChannelId::kChannelScoData, "SCO Data");
}

zx_status_t BtHciMarvell::BtHciOpenSnoopChannel(zx::channel channel) {
  return ZX_ERR_NOT_SUPPORTED;
}

void BtHciMarvell::BtHciConfigureSco(sco_coding_format_t coding_format, sco_encoding_t encoding,
                                     sco_sample_rate_t sample_rate,
                                     bt_hci_configure_sco_callback callback, void* cookie) {}

void BtHciMarvell::BtHciResetSco(bt_hci_reset_sco_callback callback, void* cookie) {}

}  // namespace bt_hci_marvell

static constexpr zx_driver_ops_t bt_hci_marvell_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = bt_hci_marvell::BtHciMarvell::Bind;
  return ops;
}();

ZIRCON_DRIVER(bt_hci_marvell, bt_hci_marvell_driver_ops, "zircon", "0.1");
