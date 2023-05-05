// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-peripheral-test/driver.h"

#include <assert.h>
#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/hw/arch_ops.h>
#include <lib/sync/completion.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <threads.h>
#include <zircon/listnode.h>
#include <zircon/process.h>
#include <zircon/syscalls.h>

#include <optional>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>
#include <usb/peripheral-test.h>
#include <usb/peripheral.h>

namespace usb_function_test {

zx_status_t UsbTest::Init() {
  zx_status_t status = device_get_protocol(parent(), ZX_PROTOCOL_USB_FUNCTION, &function_);
  if (status != ZX_OK) {
    return status;
  }

  parent_req_size_ = function_.GetRequestSize();

  status = function_.AllocInterface(&descriptors_.intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: usb_function_alloc_interface failed", __func__);
    return status;
  }

  status = function_.AllocEp(USB_DIR_OUT, &bulk_out_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: usb_function_alloc_ep failed", __func__);
    return status;
  }
  status = function_.AllocEp(USB_DIR_IN, &bulk_in_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: usb_function_alloc_ep failed", __func__);
    return status;
  }
  status = function_.AllocEp(USB_DIR_OUT, &intr_out_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: usb_function_alloc_ep failed", __func__);
    return status;
  }
  status = function_.AllocEp(USB_DIR_IN, &intr_in_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: usb_function_alloc_ep failed", __func__);
    return status;
  }

  descriptors_.bulk_out_ep.b_endpoint_address = bulk_out_addr_;
  descriptors_.bulk_in_ep.b_endpoint_address = bulk_in_addr_;
  descriptors_.intr_out_ep.b_endpoint_address = intr_out_addr_;
  descriptors_.intr_in_ep.b_endpoint_address = intr_in_addr_;

  // Allocate bulk out usb requests.
  std::optional<usb::Request<void>> req;
  fbl::AutoLock lock(&lock_);
  for (size_t i = 0; i < BULK_TX_COUNT; i++) {
    status = usb::Request<void>::Alloc(&req, BULK_REQ_SIZE, bulk_out_addr_, parent_req_size_);
    if (status != ZX_OK) {
      return status;
    }
    bulk_out_reqs_.push_next(std::move(*req));
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }
  // Allocate bulk in usb requests.
  for (size_t i = 0; i < BULK_RX_COUNT; i++) {
    status = usb::Request<void>::Alloc(&req, BULK_REQ_SIZE, bulk_in_addr_, parent_req_size_);
    if (status != ZX_OK) {
      return status;
    }
    bulk_in_reqs_.push_next(std::move(*req));
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }

  // Allocate interrupt out requests.
  for (size_t i = 0; i < INTR_COUNT; i++) {
    status = usb::Request<void>::Alloc(&req, INTR_REQ_SIZE, intr_out_addr_, parent_req_size_);
    if (status != ZX_OK) {
      return status;
    }
    intr_out_reqs_.push_next(std::move(*req));
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }
  // Allocate interrupt in requests.
  for (size_t i = 0; i < INTR_COUNT; i++) {
    status = usb::Request<void>::Alloc(&req, INTR_REQ_SIZE, intr_in_addr_, parent_req_size_);
    if (status != ZX_OK) {
      return status;
    }
    intr_in_reqs_.push_next(std::move(*req));
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }

  DdkAdd("usb-function-test", DEVICE_ADD_NON_BINDABLE);

  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: add_device failed %d", __func__, status);
    return status;
  }

  function_.SetInterface(this, &usb_function_interface_protocol_ops_);

  return ZX_OK;
}

void UsbTest::TestIntrInComplete(usb_request_t* req) {
  zxlogf(SERIAL, "%s %d %ld", __func__, req->response.status, req->response.actual);
  if (suspending_) {
    usb_request_release(req);
    return;
  }
  fbl::AutoLock lock(&lock_);
  intr_in_reqs_.push(usb::Request<void>(req, parent_req_size_));
}

void UsbTest::TestIntrOutComplete(usb_request_t* req) {
  zxlogf(SERIAL, "%s %d %ld", __func__, req->response.status, req->response.actual);

  if (suspending_) {
    usb_request_release(req);
    return;
  }
  if (req->response.status == ZX_ERR_IO_NOT_PRESENT) {
    fbl::AutoLock lock(&lock_);
    intr_out_reqs_.push_next(usb::Request<void>(req, parent_req_size_));
    return;
  }
  if (req->response.status == ZX_OK) {
    lock_.Acquire();
    std::optional<usb::Request<void>> in_req = intr_in_reqs_.pop();
    lock_.Release();
    if (in_req) {
      // Send data back to host.
      void* buffer;
      usb_request_mmap(req, &buffer);
      size_t result = in_req->CopyTo(buffer, req->response.actual, 0);
      ZX_ASSERT(result == req->response.actual);
      req->header.length = req->response.actual;

      usb_request_complete_callback_t complete = {
          .callback =
              [](void* ctx, usb_request_t* req) {
                static_cast<UsbTest*>(ctx)->TestIntrInComplete(req);
              },
          .ctx = this,
      };
      hw_mb();
      usb_request_cache_flush(in_req->request(), 0, in_req->request()->response.actual);
      function_.RequestQueue(in_req->take(), &complete);
    } else {
      zxlogf(ERROR, "%s: no intr in request available", __func__);
    }
  } else {
    zxlogf(ERROR, "%s: usb_read_complete called with status %d", __func__, req->response.status);
  }

  // Requeue read.
  usb_request_complete_callback_t complete = {
      .callback = [](void* ctx,
                     usb_request_t* req) { static_cast<UsbTest*>(ctx)->TestIntrOutComplete(req); },
      .ctx = this,
  };
  function_.RequestQueue(req, &complete);
}

void UsbTest::TestBulkOutComplete(usb_request_t* req) {
  zxlogf(SERIAL, "%s %d %ld", __func__, req->response.status, req->response.actual);
  if (suspending_) {
    usb_request_release(req);
    return;
  }
  if (req->response.status == ZX_ERR_IO_NOT_PRESENT) {
    fbl::AutoLock lock(&lock_);
    bulk_out_reqs_.push_next(usb::Request<void>(req, parent_req_size_));
    return;
  }
  if (req->response.status == ZX_OK) {
    lock_.Acquire();
    std::optional<usb::Request<void>> in_req = bulk_in_reqs_.pop();
    lock_.Release();
    if (in_req) {
      // Send data back to host.
      void* buffer;
      usb_request_mmap(req, &buffer);
      size_t result = (*in_req).CopyTo(buffer, req->response.actual, 0);
      ZX_ASSERT(result == req->response.actual);
      req->header.length = req->response.actual;

      usb_request_complete_callback_t complete = {
          .callback =
              [](void* ctx, usb_request_t* req) {
                static_cast<UsbTest*>(ctx)->TestBulkInComplete(req);
              },
          .ctx = this,
      };
      hw_mb();
      usb_request_cache_flush(in_req->request(), 0, in_req->request()->response.actual);
      function_.RequestQueue(in_req->take(), &complete);
    } else {
      zxlogf(ERROR, "%s: no bulk in request available", __func__);
    }
  } else {
    zxlogf(ERROR, "%s: usb_read_complete called with status %d", __func__, req->response.status);
  }

  // Requeue read.
  usb_request_complete_callback_t complete = {
      .callback = [](void* ctx,
                     usb_request_t* req) { static_cast<UsbTest*>(ctx)->TestBulkOutComplete(req); },
      .ctx = this,
  };
  function_.RequestQueue(req, &complete);
}

void UsbTest::TestBulkInComplete(usb_request_t* req) {
  zxlogf(SERIAL, "%s %d %ld", __func__, req->response.status, req->response.actual);
  auto req_managed = usb::Request<void>(req, parent_req_size_);
  if (suspending_) {
    return;
  }
  fbl::AutoLock lock(&lock_);
  bulk_in_reqs_.push(std::move(req_managed));
}

void UsbTest::UsbFunctionInterfaceGetDescriptors(uint8_t* buffer, size_t buffer_size,
                                                 size_t* out_actual) {
  size_t length = sizeof(descriptors_);
  if (length > buffer_size) {
    length = buffer_size;
  }
  memcpy(buffer, &descriptors_, length);
  *out_actual = length;
}

zx_status_t UsbTest::UsbFunctionInterfaceControl(const usb_setup_t* setup,
                                                 const uint8_t* write_buffer, size_t write_size,
                                                 uint8_t* read_buffer, size_t read_size,
                                                 size_t* out_read_actual) {
  size_t length = le16toh(setup->w_length);

  zxlogf(DEBUG, "%s", __func__);
  if (setup->bm_request_type == (USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE) &&
      setup->b_request == USB_PERIPHERAL_TEST_SET_DATA) {
    if (length > sizeof(test_data_)) {
      length = sizeof(test_data_);
    }
    memcpy(test_data_, write_buffer, length);
    test_data_length_ = length;
    return ZX_OK;
  } else if (setup->bm_request_type == (USB_DIR_IN | USB_TYPE_VENDOR | USB_RECIP_INTERFACE) &&
             setup->b_request == USB_PERIPHERAL_TEST_GET_DATA) {
    if (length > test_data_length_) {
      length = test_data_length_;
    }
    memcpy(read_buffer, test_data_, length);
    *out_read_actual = length;
    return ZX_OK;
  } else if (setup->bm_request_type == (USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE) &&
             setup->b_request == USB_PERIPHERAL_TEST_SEND_INTERUPT) {
    lock_.Acquire();
    std::optional<usb::Request<void>> req = intr_in_reqs_.pop();
    lock_.Release();
    if (!req) {
      zxlogf(ERROR, "%s: no interrupt request available", __func__);
      // TODO(voydanoff) maybe stall in this case?
      return ZX_OK;
    }

    size_t result = req->CopyTo(test_data_, test_data_length_, 0);
    ZX_ASSERT(result == test_data_length_);
    req->request()->header.length = test_data_length_;

    usb_request_complete_callback_t complete = {
        .callback = [](void* ctx,
                       usb_request_t* req) { static_cast<UsbTest*>(ctx)->TestIntrInComplete(req); },
        .ctx = this,
    };
    hw_mb();
    usb_request_cache_flush(req->request(), 0, req->request()->response.actual);
    function_.RequestQueue(req->take(), &complete);
    return ZX_OK;
  } else if (setup->bm_request_type == (USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE) &&
             setup->b_request == USB_PERIPHERAL_TEST_BULK_TRANSFER_SIZE) {
    if (sizeof(uint64_t) != write_size) {
      zxlogf(ERROR, "Unexpected argument size %lu should be %lu", write_size, sizeof(uint64_t));
      return ZX_ERR_INVALID_ARGS;
    }
    uint64_t buffer_size = *reinterpret_cast<const uint64_t*>(write_buffer);
    if (buffer_size >= 4096) {
      zxlogf(ERROR, "Buffer size is too big, buffer size = %lu", buffer_size);
      return ZX_ERR_INVALID_ARGS;
    }

    std::unique_ptr<uint8_t[]> size_buffer = std::make_unique<uint8_t[]>(buffer_size);
    memset(size_buffer.get(), 0x1, buffer_size);

    // Creating a bulk out request since the test starts in the driver side.
    std::optional<usb::Request<void>> req_out;
    zx_status_t status =
        usb::Request<void>::Alloc(&req_out, BULK_REQ_SIZE, bulk_out_addr_, parent_req_size_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: allocating bulk request is unavailable", __func__);
      return ZX_ERR_NOT_SUPPORTED;
    }

    req_out->request()->header.length = buffer_size;
    size_t result_out = req_out->CopyTo(size_buffer.get(), buffer_size, 0);
    ZX_ASSERT(result_out == BULK_REQ_SIZE);
    sync_completion_t call_completion;

    usb_request_complete_callback_t complete_out = {
        .callback =
            [](void* ctx, usb_request_t* req) {
              sync_completion_signal(static_cast<sync_completion_t*>(ctx));
            },
        .ctx = &call_completion,
    };
    function_.RequestQueue(req_out->take(), &complete_out);
    sync_completion_wait(&call_completion, ZX_TIME_INFINITE);

    // Obtaining a bulk in request.
    lock_.Acquire();
    std::optional<usb::Request<void>> req_in = bulk_in_reqs_.pop();
    lock_.Release();
    if (!req_in) {
      zxlogf(ERROR, "%s: No valid bulk_in request", __func__);
      return ZX_ERR_NOT_SUPPORTED;
    }
    size_t result_in = req_in->CopyTo(size_buffer.get(), buffer_size, 0);
    ZX_ASSERT(result_in == BULK_REQ_SIZE);

    usb_request_complete_callback_t complete_in = {
        .callback = [](void* ctx,
                       usb_request_t* req) { static_cast<UsbTest*>(ctx)->TestBulkInComplete(req); },
        .ctx = this,
    };
    hw_mb();
    usb_request_cache_flush(req_in->request(), 0, req_in->request()->header.length);
    function_.RequestQueue(req_in->take(), &complete_in);
    return ZX_OK;
  } else {
    return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t UsbTest::UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed) {
  zxlogf(DEBUG, "%s: %d %d", __func__, configured, speed);
  zx_status_t status;

  if (configured) {
    if ((status = function_.ConfigEp(&descriptors_.intr_in_ep, NULL)) != ZX_OK ||
        (status = function_.ConfigEp(&descriptors_.intr_out_ep, NULL)) != ZX_OK ||
        (status = function_.ConfigEp(&descriptors_.bulk_out_ep, NULL)) != ZX_OK ||
        (status = function_.ConfigEp(&descriptors_.bulk_in_ep, NULL)) != ZX_OK) {
      zxlogf(ERROR, "%s: function_.ConfigEp( failed", __func__);
      return status;
    }
  } else {
    function_.DisableEp(bulk_out_addr_);
    function_.DisableEp(bulk_in_addr_);
    function_.DisableEp(intr_out_addr_);
    function_.DisableEp(intr_in_addr_);
  }
  configured_ = configured;

  if (configured) {
    // Queue our OUT requests.
    fbl::AutoLock lock(&lock_);
    std::optional<usb::Request<void>> req;
    while ((req = bulk_out_reqs_.pop())) {
      usb_request_complete_callback_t complete = {
          .callback =
              [](void* ctx, usb_request_t* req) {
                static_cast<UsbTest*>(ctx)->TestBulkOutComplete(req);
              },
          .ctx = this,
      };
      function_.RequestQueue(req->take(), &complete);
    }

    while ((req = intr_out_reqs_.pop())) {
      usb_request_complete_callback_t complete = {
          .callback =
              [](void* ctx, usb_request_t* req) {
                static_cast<UsbTest*>(ctx)->TestIntrOutComplete(req);
              },
          .ctx = this,
      };
      function_.RequestQueue(req->take(), &complete);
    }
  }

  return ZX_OK;
}

zx_status_t UsbTest::UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting) {
  return ZX_ERR_NOT_SUPPORTED;
}

void UsbTest::DdkSuspend(ddk::SuspendTxn txn) {
  // Set suspend bit so that all requests are free'd when complete
  suspending_ = true;
  function_.CancelAll(bulk_out_addr_);
  function_.CancelAll(intr_out_addr_);
  function_.CancelAll(intr_in_addr_);
  function_.CancelAll(bulk_in_addr_);
  txn.Reply(ZX_OK, 0);
}

void UsbTest::DdkUnbind(ddk::UnbindTxn txn) {
  zxlogf(DEBUG, "%s", __func__);
  txn.Reply();
}

void UsbTest::DdkRelease() {
  zxlogf(DEBUG, "%s", __func__);
  delete this;
}

zx_status_t UsbTest::Create(void* ctx, zx_device_t* parent) {
  zxlogf(INFO, "%s", __func__);
  fbl::AllocChecker ac;
  std::unique_ptr<UsbTest> test(new (&ac) UsbTest(parent));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  zx_status_t status = test->Init();
  if (status != ZX_OK) {
    return status;
  }
  // The DDK now owns the test.
  [[maybe_unused]] UsbTest* unused = test.release();
  return ZX_OK;
}

zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = UsbTest::Create;
  return ops;
}();

}  // namespace usb_function_test

// clang-format off
ZIRCON_DRIVER(usb_function_test, usb_function_test::driver_ops, "zircon", "0.1");
// clang-format on
