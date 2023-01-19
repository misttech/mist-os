// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/108279) - remove this file

#include "magma/magma.h"

magma_status_t magma_read_notification_channel2(magma_connection_t connection, void* buffer,
                                                uint64_t buffer_size, uint64_t* buffer_size_out,
                                                magma_bool_t* more_data_out) {
  return magma_connection_read_notification_channel(connection, buffer, buffer_size,
                                                    buffer_size_out, more_data_out);
}
