// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_LIB_USB_INCLUDE_USB_SDK_REQUEST_FIDL_H_
#define SRC_DEVICES_USB_LIB_USB_INCLUDE_USB_SDK_REQUEST_FIDL_H_

#include <lib/dma-buffer/phys-iter.h>

#include "src/devices/usb/lib/usb/include/usb/internal/request-fidl.h"

namespace usb {

// Exposed to header users.
using internal::EndpointType;
using internal::MappedVmo;

using FidlRequest = internal::FidlRequest<dma_buffer::PhysIter>;
using FidlRequestPool = internal::FidlRequestPool<FidlRequest>;

// Template specialization for internal::FidlRequest<dma_buffer::PhysIter>.
template <>
inline dma_buffer::PhysIter FidlRequest::phys_iter(size_t idx, size_t max_length) const {
  ZX_ASSERT(request_.data()->at(idx).size());
  ZX_ASSERT(pinned_vmos_.find(idx) != pinned_vmos_.end());
  auto length = *request_.data()->at(idx).size();
  auto offset = *request_.data()->at(idx).offset();
  return dma_buffer::PhysIter{pinned_vmos_.at(idx).phys_list, pinned_vmos_.at(idx).phys_count,
                              offset, length, max_length};
}

}  // namespace usb

#endif  // SRC_DEVICES_USB_LIB_USB_INCLUDE_USB_SDK_REQUEST_FIDL_H_
