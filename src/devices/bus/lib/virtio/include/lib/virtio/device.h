// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_BUS_LIB_VIRTIO_INCLUDE_LIB_VIRTIO_DEVICE_H_
#define SRC_DEVICES_BUS_LIB_VIRTIO_INCLUDE_LIB_VIRTIO_DEVICE_H_

#include <lib/virtio/backends/backend.h>
#include <zircon/types.h>

#include <fbl/mutex.h>
#include <ktl/atomic.h>
#include <virtio/virtio.h>

// Virtio devices are represented by a derived class specific to their type (eg
// gpu) with a virtio::Device base. The device class handles general work around
// IRQ handling and contains a backend that is instantiated at creation time
// that implements a virtio backend. This allows a single device driver to work
// on both Virtio legacy or transistional without needing to special case the
// device interaction.
namespace virtio {

class Device {
 public:
  Device(fbl::RefPtr<BusTransactionInitiatorDispatcher> bti, ktl::unique_ptr<Backend> backend);
  virtual ~Device();

  virtual zx_status_t Init() = 0;
  virtual void Release();

  void StartIrqThread();
  // interrupt cases that devices may override
  pcie_irq_mode_t InterruptMode() { return backend_->InterruptMode(); }
  virtual void IrqRingUpdate() = 0;
  virtual void IrqConfigChange() = 0;

  // Get the Ring size for the particular device / backend.
  // This has to be proxied to a backend method because we can't
  // simply do config reads to determine the information.
  uint16_t GetRingSize(uint16_t index) { return backend_->GetRingSize(index); }
  // Set up ring descriptors with the backend.
  zx_status_t SetRing(uint16_t index, uint16_t count, zx_paddr_t pa_desc, zx_paddr_t pa_avail,
                      zx_paddr_t pa_used) {
    return backend_->SetRing(index, count, pa_desc, pa_avail, pa_used);
  }

  // Another method that has to be proxied to the backend due to differences
  // in how Legacy vs Modern systems are laid out.
  void RingKick(uint16_t ring_index) { backend_->RingKick(ring_index); }

  // It is expected that each derived device will implement tag().
  virtual const char* tag() const = 0;  // Implemented by derived devices

 protected:
  // Methods for checking / acknowledging features
  uint64_t DeviceFeaturesSupported() { return backend_->ReadFeatures(); }
  void DriverFeaturesAck(uint64_t feature_bitmap) { backend_->SetFeatures(feature_bitmap); }
  bool DeviceStatusFeaturesOk() { return backend_->ConfirmFeatures(); }

  // Device lifecycle methods
  void DeviceReset() { backend_->DeviceReset(); }
  void WaitForDeviceReset() { backend_->WaitForDeviceReset(); }
  void DriverStatusAck() { backend_->DriverStatusAck(); }
  void DriverStatusOk() { backend_->DriverStatusOk(); }
  uint32_t IsrStatus() const { return backend_->IsrStatus(); }

  // Device config management
  void CopyDeviceConfig(void* _buf, size_t len) const;
  template <typename T>
  void ReadDeviceConfig(uint16_t offset, T* val) {
    backend_->ReadDeviceConfig(offset, val);
  }
  template <typename T>
  void WriteDeviceConfig(uint16_t offset, T val) {
    backend_->WriteDeviceConfig(offset, val);
  }

  static int IrqThreadEntry(void* arg);
  void IrqWorker();

  // BTI for managing DMA
  fbl::RefPtr<BusTransactionInitiatorDispatcher> bti_;

  // backend responsible for hardware io. Will be released when device goes out of scope
  ktl::unique_ptr<Backend> backend_;
  // irq thread object
  Thread* irq_thread_ = nullptr;

  // This lock exists for devices to synchronize themselves, it should not be used by the base
  // device class.
  DECLARE_MUTEX(Device) lock_;

  ktl::atomic<bool> irq_thread_should_exit_ = false;
};

}  // namespace virtio

#endif  // SRC_DEVICES_BUS_LIB_VIRTIO_INCLUDE_LIB_VIRTIO_DEVICE_H_
