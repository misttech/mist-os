// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FUCHSIA_CONTROLLER_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FUCHSIA_CONTROLLER_H_
// LINT.IfChange
#include <stdint.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#ifdef __cplusplus
extern "C" {
#endif  // __cplusplus

struct ffx_lib_context_t;
struct ffx_env_context_t;

typedef struct _ffx_config_t {
  const char* key;
  const char* value;
} ffx_config_t;

extern void create_ffx_lib_context(ffx_lib_context_t** ctx, char* error_scratch, uint64_t size);

extern zx_status_t create_ffx_env_context(ffx_env_context_t** env_ctx, ffx_lib_context_t* lib_ctx,
                                          ffx_config_t* config, uint64_t config_len);

extern void destroy_ffx_env_context(ffx_env_context_t* ctx);

extern void destroy_ffx_lib_context(ffx_lib_context_t* env_ctx);

extern zx_status_t ffx_open_daemon_protocol(ffx_env_context_t* ctx, const char* protocol,
                                            zx_handle_t* out);

// These three functions are for convenience, and can be done via the daemon protocol
// if desired, albeit through more than one proxy layer.
extern zx_status_t ffx_open_target_proxy(ffx_env_context_t* ctx, zx_handle_t* out);
extern zx_status_t ffx_open_remote_control_proxy(ffx_env_context_t* ctx, zx_handle_t* out);
extern zx_status_t ffx_open_device_proxy(ffx_env_context_t* ctx, const char* moniker,
                                         const char* capability_name, zx_handle_t* out);
extern void ffx_close_handle(zx_handle_t handle);
extern zx_status_t ffx_channel_write(ffx_lib_context_t* ctx, zx_handle_t handle,
                                     const char* out_buf, uint64_t out_len, zx_handle_t* hdls,
                                     uint64_t hdls_len);
extern zx_status_t ffx_channel_read(ffx_lib_context_t* ctx, zx_handle_t handle, char* out_buf,
                                    uint64_t out_len, zx_handle_t* hdls, uint64_t hdls_len,
                                    uint64_t* actual_bytes_count, uint64_t* actual_handles_count);
extern zx_status_t ffx_socket_read(ffx_lib_context_t* ctx, zx_handle_t handle, char* out_buf,
                                   uint64_t out_len, uint64_t* bytes_read);
extern zx_status_t ffx_socket_write(ffx_lib_context_t* ctx, zx_handle_t handle, const char* buf,
                                    uint64_t buf_len);

// Opens a file descriptor that delivers zircon handle numbers that are ready to be read.
// There can only be one file descriptor for the lifetime of a library module, so all calls to this
// function will return the same file descriptor number.
extern int32_t ffx_open_handle_notifier(ffx_lib_context_t* ctx);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

// LINT.ThenChange(../src/lib.rs)
#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FUCHSIA_CONTROLLER_H_
