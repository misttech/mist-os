// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gbm.h"

// These stubs allow us to build Linux tests in the Fuchsia tree that dynamically link
// against libgbm.so provided by the Linux system.

int gbm_device_get_fd(struct gbm_device *gbm) { return -1; }

const char *gbm_device_get_backend_name(struct gbm_device *gbm) { return "stub"; }

int gbm_device_is_format_supported(struct gbm_device *gbm, uint32_t format, uint32_t flags) {
  return 0;
}

int gbm_device_get_format_modifier_plane_count(struct gbm_device *gbm, uint32_t format,
                                               uint64_t modifier) {
  return 0;
}

void gbm_device_destroy(struct gbm_device *gbm) {}

struct gbm_device *gbm_create_device(int fd) { return NULL; }

struct gbm_bo *gbm_bo_create(struct gbm_device *gbm, uint32_t width, uint32_t height,
                             uint32_t format, uint32_t flags) {
  return NULL;
}

struct gbm_bo *gbm_bo_create_with_modifiers(struct gbm_device *gbm, uint32_t width, uint32_t height,
                                            uint32_t format, const uint64_t *modifiers,
                                            const unsigned int count) {
  return NULL;
}

struct gbm_bo *gbm_bo_create_with_modifiers2(struct gbm_device *gbm, uint32_t width,
                                             uint32_t height, uint32_t format,
                                             const uint64_t *modifiers, const unsigned int count,
                                             uint32_t flags) {
  return NULL;
}

struct gbm_bo *gbm_bo_import(struct gbm_device *gbm, uint32_t type, void *buffer, uint32_t flags) {
  return NULL;
}

void *gbm_bo_map(struct gbm_bo *bo, uint32_t x, uint32_t y, uint32_t width, uint32_t height,
                 uint32_t flags, uint32_t *stride, void **map_data) {
  return NULL;
}

void gbm_bo_unmap(struct gbm_bo *bo, void *map_data) {}

uint32_t gbm_bo_get_width(struct gbm_bo *bo) { return 0; }

uint32_t gbm_bo_get_height(struct gbm_bo *bo) { return 0; }

uint32_t gbm_bo_get_stride(struct gbm_bo *bo) { return 0; }

uint32_t gbm_bo_get_stride_for_plane(struct gbm_bo *bo, int plane) { return 0; }

uint32_t gbm_bo_get_format(struct gbm_bo *bo) { return 0; }

uint32_t gbm_bo_get_bpp(struct gbm_bo *bo) { return 0; }

uint32_t gbm_bo_get_offset(struct gbm_bo *bo, int plane) { return 0; }

struct gbm_device *gbm_bo_get_device(struct gbm_bo *bo) { return 0; }

union gbm_bo_handle gbm_bo_get_handle(struct gbm_bo *bo) {
  union gbm_bo_handle h = {};
  return h;
}

int gbm_bo_get_fd(struct gbm_bo *bo) { return -1; }

uint64_t gbm_bo_get_modifier(struct gbm_bo *bo) { return 0; }

int gbm_bo_get_plane_count(struct gbm_bo *bo) { return 0; }

union gbm_bo_handle gbm_bo_get_handle_for_plane(struct gbm_bo *bo, int plane) {
  union gbm_bo_handle h = {};
  return h;
}

int gbm_bo_get_fd_for_plane(struct gbm_bo *bo, int plane) { return 0; }

int gbm_bo_write(struct gbm_bo *bo, const void *buf, size_t count) { return 0; }

void gbm_bo_set_user_data(struct gbm_bo *bo, void *data,
                          void (*destroy_user_data)(struct gbm_bo *, void *)) {}

void *gbm_bo_get_user_data(struct gbm_bo *bo) { return NULL; }

void gbm_bo_destroy(struct gbm_bo *bo) {}

struct gbm_surface *gbm_surface_create(struct gbm_device *gbm, uint32_t width, uint32_t height,
                                       uint32_t format, uint32_t flags) {
  return NULL;
}

struct gbm_surface *gbm_surface_create_with_modifiers(struct gbm_device *gbm, uint32_t width,
                                                      uint32_t height, uint32_t format,
                                                      const uint64_t *modifiers,
                                                      const unsigned int count) {
  return NULL;
}

struct gbm_surface *gbm_surface_create_with_modifiers2(struct gbm_device *gbm, uint32_t width,
                                                       uint32_t height, uint32_t format,
                                                       const uint64_t *modifiers,
                                                       const unsigned int count, uint32_t flags) {
  return NULL;
}

struct gbm_bo *gbm_surface_lock_front_buffer(struct gbm_surface *surface) { return NULL; }

void gbm_surface_release_buffer(struct gbm_surface *surface, struct gbm_bo *bo) {}

int gbm_surface_has_free_buffers(struct gbm_surface *surface) { return 0; }

void gbm_surface_destroy(struct gbm_surface *surface) {}

char *gbm_format_get_name(uint32_t gbm_format, struct gbm_format_name_desc *desc) {
  static char format[] = "stub";
  return format;
}
