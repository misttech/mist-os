// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_MAGMA_INCLUDE_LIB_MAGMA_SERVICE_MSD_C_H_
#define SRC_GRAPHICS_MAGMA_INCLUDE_LIB_MAGMA_SERVICE_MSD_C_H_

#include <lib/magma/magma_common_defs.h>

// This header may be used where MSD backends are implemented in C or a language
// like Rust that can build an FFI bridge from a C interface.
#ifdef __cplusplus
extern "C" {
#endif

// Driver specific - should contain language independent objects (handles)
struct MsdPlatformDevice;

struct MsdBuffer;
struct MsdDevice;
struct MsdConnection;

struct MsdDriverCallbacks {
  void (*log)(int32_t level, const char* file, int32_t line, const char* str);
};

void msd_driver_register_callbacks(struct MsdDriverCallbacks* callbacks);

struct MsdDevice* msd_driver_create_device(struct MsdPlatformDevice* platform_device);

void msd_device_release(struct MsdDevice* device);

magma_status_t msd_device_query(struct MsdDevice* device, uint64_t id,
                                magma_handle_t* result_buffer_out, uint64_t* result_out);

struct MsdConnection* msd_device_create_connection(struct MsdDevice* device, uint64_t client_id);

void msd_connection_release(struct MsdConnection* connection);

magma_status_t msd_connection_map_buffer(struct MsdConnection* msd_connection,
                                         struct MsdBuffer* msd_buffer, uint64_t gpu_va,
                                         uint64_t offset, uint64_t length, uint64_t flags);

void msd_connection_release_buffer(struct MsdConnection* msd_connection,
                                   struct MsdBuffer* msd_buffer);

struct MsdBuffer* msd_driver_import_buffer(magma_handle_t buffer_handle, uint64_t client_id);

void msd_buffer_release(struct MsdBuffer* msd_buffer);

#ifdef __cplusplus
}
#endif

#endif /* SRC_GRAPHICS_MAGMA_INCLUDE_LIB_MAGMA_SERVICE_MSD_C_H_ */
