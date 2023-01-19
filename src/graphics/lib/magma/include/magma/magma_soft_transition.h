// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/108279) remove this file after soft transition

// This interface needs an address to soft transition mesa
magma_status_t magma_read_notification_channel2(magma_connection_t connection, void* buffer,
                                                uint64_t buffer_size, uint64_t* buffer_size_out,
                                                magma_bool_t* more_data_out);

static inline magma_status_t magma_query(magma_device_t device, uint64_t id,
                                         magma_handle_t* result_buffer_out, uint64_t* result_out) {
  return magma_device_query(device, id, result_buffer_out, result_out);
}

static inline magma_status_t magma_import_semaphore(magma_connection_t connection,
                                                    magma_handle_t semaphore_handle,
                                                    magma_semaphore_t* semaphore_out) {
  return magma_connection_import_semaphore(connection, semaphore_handle, semaphore_out);
}

static inline magma_status_t magma_create_semaphore(magma_connection_t connection,
                                                    magma_semaphore_t* semaphore_out) {
  return magma_connection_create_semaphore(connection, semaphore_out);
}

static inline uint64_t magma_get_semaphore_id(magma_semaphore_t semaphore) {
  return magma_semaphore_get_id(semaphore);
}

static inline void magma_signal_semaphore(magma_semaphore_t semaphore) {
  magma_semaphore_signal(semaphore);
}

static inline void magma_release_semaphore(magma_connection_t connection,
                                           magma_semaphore_t semaphore) {
  magma_connection_release_semaphore(connection, semaphore);
}

static inline void magma_reset_semaphore(magma_semaphore_t semaphore) {
  magma_semaphore_reset(semaphore);
}

static inline magma_status_t magma_export_semaphore(magma_connection_t connection,
                                                    magma_semaphore_t semaphore,
                                                    magma_handle_t* semaphore_handle_out) {
  return magma_connection_export_semaphore(connection, semaphore, semaphore_handle_out);
}

static inline magma_status_t magma_create_connection2(magma_device_t device,
                                                      magma_connection_t* connection_out) {
  return magma_device_create_connection(device, connection_out);
}

static inline void magma_release_connection(magma_connection_t connection) {
  return magma_connection_release(connection);
}

static inline magma_status_t magma_create_buffer(magma_connection_t connection, uint64_t size,
                                                 uint64_t* size_out, magma_buffer_t* buffer_out) {
  return magma_connection_create_buffer(connection, size, size_out, buffer_out);
}

static inline void magma_release_buffer(magma_connection_t connection, magma_buffer_t buffer) {
  return magma_connection_release_buffer(connection, buffer);
}

static inline magma_status_t magma_get_buffer_handle2(magma_buffer_t buffer,
                                                      magma_handle_t* handle_out) {
  return magma_buffer_get_handle(buffer, handle_out);
}

static inline magma_status_t magma_create_context(magma_connection_t connection,
                                                  uint32_t* context_id_out) {
  return magma_connection_create_context(connection, context_id_out);
}

static inline void magma_release_context(magma_connection_t connection, uint32_t context_id) {
  return magma_connection_release_context(connection, context_id);
}

static inline magma_status_t magma_export(magma_connection_t connection, magma_buffer_t buffer,
                                          magma_handle_t* buffer_handle_out) {
  return magma_connection_export_buffer(connection, buffer, buffer_handle_out);
}

static inline magma_status_t magma_import(magma_connection_t connection,
                                          magma_handle_t buffer_handle,
                                          magma_buffer_t* buffer_out) {
  return magma_connection_import_buffer(connection, buffer_handle, buffer_out);
}

static inline magma_handle_t magma_get_notification_channel_handle(magma_connection_t connection) {
  return magma_connection_get_notification_channel_handle(connection);
}

static inline uint64_t magma_get_buffer_size(magma_buffer_t buffer) {
  return magma_buffer_get_size(buffer);
}

static inline uint64_t magma_get_buffer_id(magma_buffer_t buffer) {
  return magma_buffer_get_id(buffer);
}

static inline magma_status_t magma_map_buffer(magma_connection_t connection, uint64_t hw_va,
                                              magma_buffer_t buffer, uint64_t offset,
                                              uint64_t length, uint64_t map_flags) {
  return magma_connection_map_buffer(connection, hw_va, buffer, offset, length, map_flags);
}

static inline void magma_unmap_buffer(magma_connection_t connection, uint64_t hw_va,
                                      magma_buffer_t buffer) {
  return magma_connection_unmap_buffer(connection, hw_va, buffer);
}

static inline magma_status_t magma_virt_create_image(magma_connection_t connection,
                                                     magma_image_create_info_t* create_info,
                                                     magma_buffer_t* image_out) {
  return magma_virt_connection_create_image(connection, create_info, image_out);
}

static inline magma_status_t magma_execute_command(magma_connection_t connection,
                                                   uint32_t context_id,
                                                   struct magma_command_descriptor* descriptor) {
  return magma_connection_execute_command(connection, context_id, descriptor);
}

static inline magma_status_t magma_clean_cache(magma_buffer_t buffer, uint64_t offset,
                                               uint64_t size, magma_cache_operation_t operation) {
  return magma_buffer_clean_cache(buffer, offset, size, operation);
}

static inline magma_status_t magma_virt_get_image_info(magma_connection_t connection,
                                                       magma_buffer_t image,
                                                       magma_image_info_t* image_info_out) {
  return magma_virt_connection_get_image_info(connection, image, image_info_out);
}
