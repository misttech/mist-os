// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/dwc3/dwc3.h"

#include <fidl/fuchsia.hardware.usb.descriptor/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/fit/defer.h>
#include <lib/zx/clock.h>

#include <cstdlib>
#include <span>

#include <fbl/auto_lock.h>
#include <usb/usb.h>

#include "src/devices/usb/drivers/dwc3/dwc3-regs.h"

namespace dwc3 {

namespace fdescriptor = fuchsia_hardware_usb_descriptor;

zx_status_t Dwc3::Create(void* ctx, zx_device_t* parent) {
  auto dev = std::make_unique<Dwc3>(parent);
  if (zx_status_t status = dev->AcquirePDevResources(); status != ZX_OK) {
    zxlogf(ERROR, "Dwc3 Create failed (%s)", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status =
          dev->DdkAdd(ddk::DeviceAddArgs("dwc3")
                          .forward_metadata(parent, DEVICE_METADATA_MAC_ADDRESS)
                          .forward_metadata(parent, DEVICE_METADATA_SERIAL_NUMBER));
      status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %s", zx_status_get_string(status));
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* _ = dev.release();
  return ZX_OK;
}

zx_status_t Dwc3::AcquirePDevResources() {
  pdev_ = ddk::PDevFidl::FromFragment(parent());
  if (!pdev_.is_valid()) {
    zxlogf(ERROR, "Could not get platform device protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (zx_status_t status = pdev_.MapMmio(0, &mmio_); status != ZX_OK) {
    zxlogf(ERROR, "MapMmio failed: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = pdev_.GetBti(0, &bti_); status != ZX_OK) {
    zxlogf(ERROR, "GetBti failed: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = pdev_.GetInterrupt(0, 0, &irq_); status != ZX_OK) {
    zxlogf(ERROR, "GetInterrupt failed: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = zx::port::create(ZX_PORT_BIND_TO_INTERRUPT, &irq_port_);
      status != ZX_OK) {
    zxlogf(ERROR, "zx::port::create failed: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = irq_.bind(irq_port_, 0, ZX_INTERRUPT_BIND); status != ZX_OK) {
    zxlogf(ERROR, "irq bind to port failed: %s", zx_status_get_string(status));
    return status;
  }
  irq_bound_to_port_ = true;

  return ZX_OK;
}

zx_status_t Dwc3::Init() {
  // Start by identifying our hardware and making sure that we recognize it, and
  // it is a version that we know we can support.  Then, reset the hardware so
  // that we know it is in a good state.
  uint32_t ep_count{0};
  {
    fbl::AutoLock lock(&lock_);

    // Now that we have our registers, check to make sure that we are running on
    // a version of the hardware that we support.
    if (zx_status_t status = CheckHwVersion(); status != ZX_OK) {
      zxlogf(ERROR, "CheckHwVersion failed: %s", zx_status_get_string(status));
      return status;
    }

    // Now that we have our registers, reset the hardware.  This will ensure that
    // we are starting from a known state moving forward.
    if (zx_status_t status = ResetHw(); status != ZX_OK) {
      zxlogf(ERROR, "HW Reset Failed: %s", zx_status_get_string(status));
      return status;
    }

    // Finally, figure out the number of endpoints that this version of the
    // controller supports.
    ep_count = GHWPARAMS3::Get().ReadFrom(get_mmio()).DWC_USB31_NUM_EPS();
  }

  if (ep_count < (kUserEndpointStartNum + 1)) {
    zxlogf(ERROR, "HW supports only %u physical endpoints, but at least %u are needed to operate.",
           ep_count, (kUserEndpointStartNum + 1));
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Now go ahead and allocate the user endpoint storage.
  user_endpoints_.Init(ep_count - kUserEndpointStartNum);

  // Now that we have our BTI, and have reset our hardware, we can go ahead and
  // release the quarantine on any pages which may have been previously pinned
  // by this BTI.
  if (zx_status_t status = bti_.release_quarantine(); status != ZX_OK) {
    zxlogf(ERROR, "Release quarantine failed: %s", zx_status_get_string(status));
    return status;
  }

  // If something goes wrong after this point, make sure to release any of our
  // allocated IoBuffers.
  auto cleanup = fit::defer([this]() { ReleaseResources(); });

  // Strictly speaking, we should not need RW access to this buffer.
  // Unfortunately, attempting to writeback and invalidate the cache before
  // reading anything from the buffer produces a page fault right if this buffer
  // is mapped read only, so for now, we keep the buffer mapped RW.
  if (zx_status_t status =
          event_buffer_.Init(bti_.get(), kEventBufferSize, IO_BUFFER_RW | IO_BUFFER_CONTIG);
      status != ZX_OK) {
    zxlogf(ERROR, "event_buffer init failed: %s", zx_status_get_string(status));
    return status;
  }
  event_buffer_.CacheFlushInvalidate(0, kEventBufferSize);

  // Now that we have allocated our event buffer, we have at least one region
  // pinned.  We need to be sure to place the hardware into reset before
  // unpinning the memory during shutdown.
  has_pinned_memory_ = true;

  {
    fbl::AutoLock lock(&ep0_.lock);
    if (zx_status_t status =
            ep0_.buffer.Init(bti_.get(), UINT16_MAX, IO_BUFFER_RW | IO_BUFFER_CONTIG);
        status != ZX_OK) {
      zxlogf(ERROR, "ep0_buffer init failed: %s", zx_status_get_string(status));
      return status;
    }
  }

  if (zx_status_t status = Ep0Init(); status != ZX_OK) {
    zxlogf(ERROR, "Ep0Init init failed: %s", zx_status_get_string(status));
    return status;
  }

  // Things went well.  Cancel our cleanup routine.
  cleanup.cancel();
  return ZX_OK;
}

void Dwc3::ReleaseResources() {
  // The IRQ thread had better not be running at this point.
  ZX_ASSERT(!irq_thread_started_.load());

  // Unbind the interrupt from the interrupt port.
  if (irq_bound_to_port_) {
    irq_.bind(irq_port_, 0, ZX_INTERRUPT_UNBIND);
    irq_bound_to_port_ = false;
  }

  {
    fbl::AutoLock lock(&lock_);
    // If we managed to get our registers mapped, place the device into reset so
    // we are certain that there is no DMA going on in the background.
    if (mmio_.has_value()) {
      if (zx_status_t status = ResetHw(); status != ZX_OK) {
        // Deliberately panic and terminate this driver if we fail to place the
        // hardware into reset at this point and we have any pinned memory..  We do this
        // deliberately because, if we cannot put the hardware into reset, it may still be accessing
        // pages we previously pinned using DMA.  If we are on a system with no
        // IOMMU, deliberately terminating the process will ensure that our
        // pinned pages are quarantined instead of being returned to the page
        // pool.
        if (has_pinned_memory_) {
          zxlogf(ERROR,
                 "Failed to place HW into reset during shutdown (%s), self-terminating in order to "
                 "ensure quarantine",
                 zx_status_get_string(status));
          ZX_ASSERT(false);
        }
      }
    }
  }

  // Now go ahead and release any buffers we may have pinned.
  {
    fbl::AutoLock lock(&ep0_.lock);
    ep0_.out.enabled = false;
    ep0_.in.enabled = false;
    ep0_.buffer.release();
    ep0_.shared_fifo.Release();
  }

  for (UserEndpoint& uep : user_endpoints_) {
    fbl::AutoLock lock(&uep.ep.lock);
    uep.fifo.Release();
    uep.ep.enabled = false;
  }

  event_buffer_.release();
  has_pinned_memory_ = false;
}

zx_status_t Dwc3::CheckHwVersion() {
  auto* mmio = get_mmio();
  const uint32_t core_id = GSNPSID::Get().ReadFrom(mmio).core_id();
  if (core_id == 0x5533) {
    return ZX_OK;
  }

  const uint32_t ip_version = USB31_VER_NUMBER::Get().ReadFrom(mmio).IPVERSION();

  auto is_ascii_digit = [](char val) -> bool { return (val >= '0') && (val <= '9'); };
  auto is_ascii_letter = [](char val) -> bool {
    return ((val >= 'A') && (val <= 'Z')) || ((val >= 'a') && (val <= 'z'));
  };

  const char c1 = static_cast<char>((ip_version >> 24) & 0xFF);
  const char c2 = static_cast<char>((ip_version >> 16) & 0xFF);
  const char c3 = static_cast<char>((ip_version >> 8) & 0xFF);
  const char c4 = static_cast<char>(ip_version & 0xFF);

  // Format defined by section 1.3.44 of the DWC3 Programming Guide
  if (!is_ascii_digit(c1) || !is_ascii_digit(c2) || !is_ascii_digit(c3) ||
      (!is_ascii_letter(c4) && (c4 != '*'))) {
    zxlogf(ERROR, "Unrecognized USB IP Version 0x%08x", ip_version);
    return ZX_ERR_NOT_SUPPORTED;
  }

  const int major = c1 - '0';
  const int minor = ((c2 - '0') * 10) + (c3 - '0');

  if (major != 1) {
    zxlogf(ERROR, "Unsupported USB IP Version %d.%02d%c", major, minor, c4);
    return ZX_ERR_NOT_SUPPORTED;
  }

  zxlogf(INFO, "Detected DWC3 IP version %d.%02d%c", major, minor, c4);
  return ZX_OK;
}

zx_status_t Dwc3::ResetHw() {
  auto* mmio = get_mmio();

  // Clear the run/stop bit and request a software reset.
  DCTL::Get().ReadFrom(mmio).set_RUN_STOP(0).set_CSFTRST(1).WriteTo(mmio);

  // HW will clear the software reset bit when it is finished with the reset
  // process.
  zx::time start = zx::clock::get_monotonic();
  while (DCTL::Get().ReadFrom(mmio).CSFTRST()) {
    if ((zx::clock::get_monotonic() - start) >= kHwResetTimeout) {
      return ZX_ERR_TIMED_OUT;
    }
    usleep(1000);
  }

  return ZX_OK;
}

void Dwc3::SetDeviceAddress(uint32_t address) {
  auto* mmio = get_mmio();
  DCFG::Get().ReadFrom(mmio).set_DEVADDR(address).WriteTo(mmio);
}

void Dwc3::StartPeripheralMode() {
  {
    fbl::AutoLock lock(&lock_);
    auto* mmio = get_mmio();

    // configure and enable PHYs
    GUSB2PHYCFG::Get(0)
        .ReadFrom(mmio)
        .set_USBTRDTIM(9)    // USB2.0 Turn-around time == 9 phy clocks
        .set_ULPIAUTORES(0)  // No auto resume
        .WriteTo(mmio);

    GUSB3PIPECTL::Get(0)
        .ReadFrom(mmio)
        .set_DELAYP1TRANS(0)
        .set_SUSPENDENABLE(0)
        .set_LFPSFILTER(1)
        .set_SS_TX_DE_EMPHASIS(1)
        .WriteTo(mmio);

    // TODO(johngro): This is the number of receive buffers.  Why do we set it to 16?
    constexpr uint32_t nump = 16;
    DCFG::Get()
        .ReadFrom(mmio)
        .set_NUMP(nump)                  // number of receive buffers
        .set_DEVSPD(DCFG::DEVSPD_SUPER)  // max speed is 5Gbps USB3.1
        .set_DEVADDR(0)                  // device address is 0
        .WriteTo(mmio);

    // Program the location of the event buffer, then enable event delivery.
    StartEvents();
  }

  Ep0Start();

  {
    // Set the run/stop bit to start the controller
    fbl::AutoLock lock(&lock_);
    auto* mmio = get_mmio();
    DCTL::Get().FromValue(0).set_RUN_STOP(1).WriteTo(mmio);
  }
}

void Dwc3::ResetConfiguration() {
  {
    fbl::AutoLock lock(&lock_);
    auto* mmio = get_mmio();
    // disable all endpoints except EP0_OUT and EP0_IN
    DALEPENA::Get().FromValue(0).EnableEp(kEp0Out).EnableEp(kEp0In).WriteTo(mmio);
  }

  for (UserEndpoint& uep : user_endpoints_) {
    fbl::AutoLock lock(&uep.ep.lock);
    EpEndTransfers(uep.ep, ZX_ERR_IO_NOT_PRESENT);
    EpSetStall(uep.ep, false);
  }
}

void Dwc3::HandleResetEvent() {
  zxlogf(INFO, "Dwc3::HandleResetEvent");

  Ep0Reset();

  for (UserEndpoint& uep : user_endpoints_) {
    fbl::AutoLock lock(&uep.ep.lock);
    EpEndTransfers(uep.ep, ZX_ERR_IO_NOT_PRESENT);
    EpSetStall(uep.ep, false);
  }

  {
    fbl::AutoLock lock(&lock_);
    SetDeviceAddress(0);
  }

  Ep0Start();

  {
    fbl::AutoLock lock(&dci_lock_);
    if (dci_intf_valid()) {
      DciIntfWrapSetConnected(true);
    }
  }
}

void Dwc3::HandleConnectionDoneEvent() {
  uint16_t ep0_max_packet = 0;
  usb_speed_t new_speed = USB_SPEED_UNDEFINED;
  {
    fbl::AutoLock lock(&lock_);
    auto* mmio = get_mmio();

    uint32_t speed = DSTS::Get().ReadFrom(mmio).CONNECTSPD();

    switch (speed) {
      case DSTS::CONNECTSPD_HIGH:
        new_speed = USB_SPEED_HIGH;
        ep0_max_packet = 64;
        break;
      case DSTS::CONNECTSPD_FULL:
        new_speed = USB_SPEED_FULL;
        ep0_max_packet = 64;
        break;
      case DSTS::CONNECTSPD_SUPER:
        new_speed = USB_SPEED_SUPER;
        ep0_max_packet = 512;
        break;
      case DSTS::CONNECTSPD_ENHANCED_SUPER:
        new_speed = USB_SPEED_ENHANCED_SUPER;
        ep0_max_packet = 512;
        break;
      default:
        zxlogf(ERROR, "unsupported speed %u", speed);
        break;
    }
  }

  if (ep0_max_packet) {
    fbl::AutoLock lock(&ep0_.lock);

    std::array eps{&ep0_.out, &ep0_.in};
    for (Endpoint* ep : eps) {
      ep->type = USB_ENDPOINT_CONTROL;
      ep->interval = 0;
      ep->max_packet_size = ep0_max_packet;
      CmdEpSetConfig(*ep, true);
    }
    ep0_.cur_speed = new_speed;
  }

  {
    fbl::AutoLock lock(&dci_lock_);
    if (dci_intf_valid()) {
      DciIntfWrapSetSpeed(new_speed);
    }
  }
}

void Dwc3::HandleDisconnectedEvent() {
  zxlogf(INFO, "Dwc3::HandleDisconnectedEvent");

  {
    fbl::AutoLock ep0_lock(&ep0_.lock);
    CmdEpEndTransfer(ep0_.out);
    ep0_.state = Ep0::State::None;
  }

  {
    fbl::AutoLock lock(&dci_lock_);
    if (dci_intf_valid()) {
      DciIntfWrapSetConnected(false);
    }
  }

  for (UserEndpoint& uep : user_endpoints_) {
    fbl::AutoLock lock(&uep.ep.lock);
    EpEndTransfers(uep.ep, ZX_ERR_IO_NOT_PRESENT);
    EpSetStall(uep.ep, false);
  }
}

void Dwc3::DdkInit(ddk::InitTxn txn) {
  if (zx_status_t status = Init(); status != ZX_OK) {
    txn.Reply(status);
  } else {
    zxlogf(INFO, "Dwc3 Init Succeeded");
    txn.Reply(ZX_OK);
  }
}

void Dwc3::DdkUnbind(ddk::UnbindTxn txn) {
  if (irq_thread_started_.load()) {
    zx_status_t status = SignalIrqThread(IrqSignal::Exit);
    // if we can't signal the thread, we are not going to be able to shut down
    // and we should just terminate the process instead.
    ZX_ASSERT(status == ZX_OK);
    thrd_join(irq_thread_, nullptr);
    irq_thread_started_.store(false);
  }

  txn.Reply();
}

void Dwc3::DdkRelease() {
  ReleaseResources();
  delete this;
}

void Dwc3::UsbDciRequestQueue(usb_request_t* usb_req, const usb_request_complete_callback_t* cb) {
  Request req{usb_req, *cb, sizeof(*usb_req)};

  zx_status_t queue_result = [&]() {
    const uint8_t ep_num = UsbAddressToEpNum(req.request()->header.ep_address);
    UserEndpoint* const uep = get_user_endpoint(ep_num);

    if (uep == nullptr) {
      zxlogf(ERROR, "Dwc3::UsbDciRequestQueue: bad ep address 0x%02X", ep_num);
      return ZX_ERR_INVALID_ARGS;
    }

    const zx_off_t length = req.request()->header.length;
    zxlogf(SERIAL, "UsbDciRequestQueue ep %u length %zu", ep_num, length);

    {
      fbl::AutoLock lock(&uep->ep.lock);

      if (!uep->ep.enabled) {
        zxlogf(ERROR, "Dwc3: ep(%u) not enabled!", ep_num);
        return ZX_ERR_BAD_STATE;
      }

      // OUT transactions must have length > 0 and multiple of max packet size
      if (uep->ep.IsOutput()) {
        if (length == 0 || ((length % uep->ep.max_packet_size) != 0)) {
          zxlogf(ERROR, "Dwc3: OUT transfers must be multiple of max packet size (len %ld mps %hu)",
                 length, uep->ep.max_packet_size);
          return ZX_ERR_INVALID_ARGS;
        }
      }

      // Add the request to our queue of pending requests.  Then, if we are
      // configured, kick the queue to make sure it is running.  Do not fail the
      // request!  In particular, during the set interface callback to the CDC
      // driver, the driver will attempt to queue a request.  We are (at this
      // point in time) not _technically_ configured yet.  We declare ourselves
      // to be configured only after our call into the CDC client succeeds.  So,
      // if we fail requests because we are not yet configured yet (as the dwc2
      // driver does), we are just going to end up in the infinite recursion or
      // deadlock traps described below.
      uep->ep.queued_reqs.push(std::move(req));
      if (configured_) {
        UserEpQueueNext(*uep);
      }
      return ZX_OK;
    }
  }();

  if (queue_result != ZX_OK) {
    ZX_DEBUG_ASSERT(false);
    req.request()->response.status = queue_result;
    req.request()->response.actual = 0;
    pending_completions_.push(std::move(req));
    if (zx_status_t status = SignalIrqThread(IrqSignal::Wakeup); status != ZX_OK) {
      zxlogf(DEBUG, "Failed to signal IRQ thread %s", zx_status_get_string(status));
    }
  }
}

zx_status_t Dwc3::CommonSetInterface() {
  StartPeripheralMode();

  // Start the interrupt thread.
  auto irq_thunk = +[](void* arg) -> int { return static_cast<Dwc3*>(arg)->IrqThread(); };
  if (int rc = thrd_create_with_name(&irq_thread_, irq_thunk, static_cast<void*>(this),
                                     "dwc3-interrupt-thread");
      rc != thrd_success) {
    return ZX_ERR_INTERNAL;
  }
  irq_thread_started_.store(true);

  return ZX_OK;
}

zx_status_t Dwc3::UsbDciSetInterface(const usb_dci_interface_protocol_t* interface) {
  fbl::AutoLock lock(&dci_lock_);

  if (dci_intf_valid()) {
    zxlogf(ERROR, "%s: DCI Interface already set", __func__);
    return ZX_ERR_BAD_STATE;
  }

  dci_intf_ = ddk::UsbDciInterfaceProtocolClient(interface);

  return CommonSetInterface();
}

void Dwc3::SetInterface(SetInterfaceRequest& request, SetInterfaceCompleter::Sync& completer) {
  fbl::AutoLock lock(&dci_lock_);

  if (dci_intf_valid()) {
    zxlogf(ERROR, "%s: DCI Interface already set", __func__);
    completer.Reply(zx::error(ZX_ERR_BAD_STATE));
    return;
  }

  dci_intf_ = DciInterfaceFidlClient{};
  std::get<DciInterfaceFidlClient>(dci_intf_).Bind(std::move(request.interface()));

  if (zx_status_t status = CommonSetInterface(); status != ZX_OK) {
    completer.Reply(zx::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

zx_status_t Dwc3::CommonConfigureEndpoint(const usb_endpoint_descriptor_t* ep_desc,
                                          const usb_ss_ep_comp_descriptor_t* ss_comp_desc) {
  const uint8_t ep_num = UsbAddressToEpNum(ep_desc->b_endpoint_address);
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  uint8_t ep_type = usb_ep_type(ep_desc);
  if (ep_type == USB_ENDPOINT_ISOCHRONOUS) {
    zxlogf(ERROR, "isochronous endpoints are not supported");
    return ZX_ERR_NOT_SUPPORTED;
  }

  fbl::AutoLock lock(&uep->ep.lock);

  if (zx_status_t status = uep->fifo.Init(bti_); status != ZX_OK) {
    zxlogf(ERROR, "fifo init failed %s", zx_status_get_string(status));
    return status;
  }
  uep->ep.max_packet_size = usb_ep_max_packet(ep_desc);
  uep->ep.type = ep_type;
  uep->ep.interval = ep_desc->b_interval;
  // TODO(voydanoff) USB3 support
  uep->ep.enabled = true;
  EpSetConfig(uep->ep, true);

  // TODO(johngro): What protects this configured_ state from a locking/threading perspective?
  if (configured_) {
    UserEpQueueNext(*uep);
  }

  return ZX_OK;
}

zx_status_t Dwc3::UsbDciConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                                 const usb_ss_ep_comp_descriptor_t* ss_comp_desc) {
  return CommonConfigureEndpoint(ep_desc, ss_comp_desc);
}

void Dwc3::ConfigureEndpoint(ConfigureEndpointRequest& request,
                             ConfigureEndpointCompleter::Sync& completer) {
  // For now, we'll convert the FIDL-structs into the requisite banjo-structs. Later, when we get
  // rid of the banjo stuff, we can just use the FIDL struct field data directly.
  usb_endpoint_descriptor_t ep_desc{
      .b_length = request.ep_descriptor().b_length(),
      .b_descriptor_type = request.ep_descriptor().b_descriptor_type(),
      .b_endpoint_address = request.ep_descriptor().b_endpoint_address(),
      .bm_attributes = request.ep_descriptor().bm_attributes(),
      .w_max_packet_size = request.ep_descriptor().w_max_packet_size(),
      .b_interval = request.ep_descriptor().b_interval()};

  usb_ss_ep_comp_descriptor_t ss_comp_desc{
      .b_length = request.ss_comp_descriptor().b_length(),
      .b_descriptor_type = request.ss_comp_descriptor().b_descriptor_type(),
      .b_max_burst = request.ss_comp_descriptor().b_max_burst(),
      .bm_attributes = request.ss_comp_descriptor().bm_attributes(),
      .w_bytes_per_interval = request.ss_comp_descriptor().w_bytes_per_interval()};

  if (zx_status_t status = CommonConfigureEndpoint(&ep_desc, &ss_comp_desc); status != ZX_OK) {
    completer.Reply(zx::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

zx_status_t Dwc3::CommonDisableEndpoint(uint8_t ep_addr) {
  const uint8_t ep_num = UsbAddressToEpNum(ep_addr);
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  RequestQueue to_complete;
  {
    fbl::AutoLock lock(&uep->ep.lock);
    to_complete = UserEpCancelAllLocked(*uep);
    uep->fifo.Release();
    uep->ep.enabled = false;
  }

  to_complete.CompleteAll(ZX_ERR_IO_NOT_PRESENT, 0);
  return ZX_OK;
}

zx_status_t Dwc3::UsbDciDisableEp(uint8_t ep_address) { return CommonDisableEndpoint(ep_address); }

void Dwc3::DisableEndpoint(DisableEndpointRequest& request,
                           DisableEndpointCompleter::Sync& completer) {
  if (zx_status_t status = CommonDisableEndpoint(request.ep_address()); status != ZX_OK) {
    completer.Reply(zx::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

zx_status_t Dwc3::CommonEndpointSetStall(uint8_t ep_addr) {
  const uint8_t ep_num = UsbAddressToEpNum(ep_addr);
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AutoLock lock(&uep->ep.lock);
  return EpSetStall(uep->ep, true);
}

zx_status_t Dwc3::UsbDciEpSetStall(uint8_t ep_address) {
  return CommonEndpointSetStall(ep_address);
}

void Dwc3::EndpointSetStall(EndpointSetStallRequest& request,
                            EndpointSetStallCompleter::Sync& completer) {
  if (zx_status_t status = CommonEndpointSetStall(request.ep_address()); status != ZX_OK) {
    completer.Reply(zx::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

zx_status_t Dwc3::CommonEndpointClearStall(uint8_t ep_addr) {
  const uint8_t ep_num = UsbAddressToEpNum(ep_addr);
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AutoLock lock(&uep->ep.lock);
  return EpSetStall(uep->ep, false);
}

zx_status_t Dwc3::UsbDciEpClearStall(uint8_t ep_address) {
  return CommonEndpointClearStall(ep_address);
}

void Dwc3::EndpointClearStall(EndpointClearStallRequest& request,
                              EndpointClearStallCompleter::Sync& completer) {
  if (zx_status_t status = CommonEndpointClearStall(request.ep_address()); status != ZX_OK) {
    completer.Reply(zx::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

size_t Dwc3::UsbDciGetRequestSize() { return Request::RequestSize(sizeof(usb_request_t)); }

zx_status_t Dwc3::CommonCancelAll(uint8_t ep_addr) {
  const uint8_t ep_num = UsbAddressToEpNum(ep_addr);
  UserEndpoint* const uep = get_user_endpoint(ep_num);

  if (uep == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  return UserEpCancelAll(*uep);
}

zx_status_t Dwc3::UsbDciCancelAll(uint8_t ep_address) { return CommonCancelAll(ep_address); }

void Dwc3::CancelAll(CancelAllRequest& request, CancelAllCompleter::Sync& completer) {
  if (zx_status_t status = CommonCancelAll(request.ep_address()); status != ZX_OK) {
    completer.Reply(zx::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

void Dwc3::DciIntfWrapSetSpeed(usb_speed_t speed) {
  ZX_ASSERT(dci_intf_valid());

  if (std::holds_alternative<DciInterfaceBanjoClient>(dci_intf_)) {
    std::get<DciInterfaceBanjoClient>(dci_intf_).SetSpeed(speed);
    return;
  }

  fdescriptor::wire::UsbSpeed fspeed{fdescriptor::wire::UsbSpeed::kUndefined};

  // Convert banjo usb_speed_t into FIDL speed struct.
  switch (speed) {
    case USB_SPEED_LOW:
      fspeed = fdescriptor::wire::UsbSpeed::kLow;
      break;
    case USB_SPEED_FULL:
      fspeed = fdescriptor::wire::UsbSpeed::kFull;
      break;
    case USB_SPEED_HIGH:
      fspeed = fdescriptor::wire::UsbSpeed::kHigh;
      break;
    case USB_SPEED_SUPER:
      fspeed = fdescriptor::wire::UsbSpeed::kSuper;
      break;
    case USB_SPEED_ENHANCED_SUPER:
      fspeed = fdescriptor::wire::UsbSpeed::kEnhancedSuper;
      break;
  };

  fidl::Arena arena;
  auto result = std::get<DciInterfaceFidlClient>(dci_intf_).buffer(arena)->SetSpeed(fspeed);
  if (!result.ok()) {
    zxlogf(ERROR, "(framework) SetSpeed(): %s", result.status_string());
  } else if (result->is_error()) {
    zxlogf(ERROR, "SetSpeed(): %s", result.error().FormatDescription().c_str());
  }
}

void Dwc3::DciIntfWrapSetConnected(bool connected) {
  ZX_ASSERT(dci_intf_valid());

  if (std::holds_alternative<DciInterfaceBanjoClient>(dci_intf_)) {
    std::get<DciInterfaceBanjoClient>(dci_intf_).SetConnected(connected);
    return;
  }

  fidl::Arena arena;
  auto result = std::get<DciInterfaceFidlClient>(dci_intf_).buffer(arena)->SetConnected(connected);
  if (!result.ok()) {
    zxlogf(ERROR, "(framework) SetConnected(): %s", result.status_string());
  } else if (result->is_error()) {
    zxlogf(ERROR, "SetConnected(): %s", result.error().FormatDescription().c_str());
  }
}

zx_status_t Dwc3::DciIntfWrapControl(const usb_setup_t* setup, const uint8_t* write_buffer,
                                     size_t write_size, uint8_t* read_buffer, size_t read_size,
                                     size_t* read_actual) {
  ZX_ASSERT(dci_intf_valid());

  if (std::holds_alternative<DciInterfaceBanjoClient>(dci_intf_)) {
    return std::get<DciInterfaceBanjoClient>(dci_intf_).Control(
        setup, write_buffer, write_size, read_buffer, read_size, read_actual);
  }

  // Convert banjo usb_setup_t into FIDL setup struct.
  fdescriptor::wire::UsbSetup fsetup;
  fsetup.bm_request_type = setup->bm_request_type;
  fsetup.b_request = setup->b_request;
  fsetup.w_value = setup->w_value;
  fsetup.w_index = setup->w_index;
  fsetup.w_length = setup->w_length;

  auto fwrite =
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(write_buffer), write_size);

  fidl::Arena arena;
  auto result = std::get<DciInterfaceFidlClient>(dci_intf_).buffer(arena)->Control(fsetup, fwrite);

  if (!result.ok()) {
    zxlogf(ERROR, "(framework) Control(): %s", result.status_string());
    return ZX_ERR_INTERNAL;
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Control(): %s", result.error().FormatDescription().c_str());
    return result->error_value();
  }

  // A lightweight byte-span is used to make it easier to process the read data.
  cpp20::span<uint8_t> read_data{result.value()->read.get()};

  // Don't blow out caller's buffer.
  if (read_data.size_bytes() > read_size) {
    return ZX_ERR_NO_MEMORY;
  }

  if (!read_data.empty()) {
    std::memcpy(read_buffer, read_data.data(), read_data.size_bytes());
    *read_actual = read_data.size_bytes();
  }

  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = Dwc3::Create;
  return ops;
}();

}  // namespace dwc3

ZIRCON_DRIVER(dwc3, dwc3::driver_ops, "zircon", "0.1");
