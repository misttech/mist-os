// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "magma/magma_sysmem.h"

#include "magma_util/macros.h"
#include "platform_sysmem_connection.h"

magma_status_t magma_sysmem_connection_import(magma_handle_t channel,
                                              magma_sysmem_connection_t* connection_out) {
  auto platform_connection = magma_sysmem::PlatformSysmemConnection::Import(channel);
  if (!platform_connection) {
    return DRET_MSG(MAGMA_STATUS_INTERNAL_ERROR, "Failed to create sysmem connection");
  }
  *connection_out = reinterpret_cast<magma_sysmem_connection_t>(platform_connection.release());
  return MAGMA_STATUS_OK;
}

void magma_sysmem_connection_release(magma_sysmem_connection_t connection) {
  delete reinterpret_cast<magma_sysmem::PlatformSysmemConnection*>(connection);
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_sysmem_allocate_buffer(magma_sysmem_connection_t connection, uint32_t flags,
                                            uint64_t size, magma_handle_t* buffer_handle_out) {
  return magma_sysmem_connection_allocate_buffer(connection, flags, size, buffer_handle_out);
}

magma_status_t magma_sysmem_connection_allocate_buffer(magma_sysmem_connection_t connection,
                                                       uint32_t flags, uint64_t size,
                                                       magma_handle_t* buffer_handle_out) {
  std::unique_ptr<magma::PlatformBuffer> buffer;
  auto sysmem_connection = reinterpret_cast<magma_sysmem::PlatformSysmemConnection*>(connection);

  magma_status_t result;
  result = sysmem_connection->AllocateBuffer(flags, size, &buffer);
  if (result != MAGMA_STATUS_OK) {
    return DRET_MSG(result, "AllocateBuffer failed: %d", result);
  }

  if (!buffer->duplicate_handle(buffer_handle_out)) {
    return DRET_MSG(MAGMA_STATUS_INTERNAL_ERROR, "duplicate_handle failed");
  }
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
void magma_buffer_format_description_release(magma_buffer_format_description_t description) {
  magma_collection_info_release(description);
}

void magma_collection_info_release(magma_collection_info_t collection_info) {
  delete reinterpret_cast<magma_sysmem::PlatformBufferDescription*>(collection_info);
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_get_buffer_format_plane_info_with_size(
    magma_buffer_format_description_t description, uint32_t width, uint32_t height,
    magma_image_plane_t* image_planes_out) {
  return magma_collection_info_get_plane_info_with_size(description, width, height,
                                                        image_planes_out);
}

// |image_planes_out| must be an array with MAGMA_MAX_IMAGE_PLANES elements.
magma_status_t magma_collection_info_get_plane_info_with_size(
    magma_collection_info_t collection_info, uint32_t width, uint32_t height,
    magma_image_plane_t* image_planes_out) {
  if (!collection_info) {
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Null collection_info");
  }
  auto buffer_collection_info =
      reinterpret_cast<magma_sysmem::PlatformBufferDescription*>(collection_info);
  if (!buffer_collection_info->GetPlanes(width, height, image_planes_out)) {
    return DRET(MAGMA_STATUS_INVALID_ARGS);
  }
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_get_buffer_format(magma_buffer_format_description_t description,
                                       uint32_t* format_out) {
  return magma_collection_info_get_format(description, format_out);
}

magma_status_t magma_collection_info_get_format(magma_collection_info_t collection_info,
                                                uint32_t* format_out) {
  if (!collection_info) {
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Null collection_info");
  }
  auto buffer_collection_info =
      reinterpret_cast<magma_sysmem::PlatformBufferDescription*>(collection_info);
  *format_out = buffer_collection_info->format();
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_get_buffer_format_modifier(magma_buffer_format_description_t description,
                                                magma_bool_t* has_format_modifier_out,
                                                uint64_t* format_modifier_out) {
  return magma_collection_info_get_format_modifier(description, has_format_modifier_out,
                                                   format_modifier_out);
}

magma_status_t magma_collection_info_get_format_modifier(magma_collection_info_t collection_info,
                                                         magma_bool_t* has_format_modifier_out,
                                                         uint64_t* format_modifier_out) {
  if (!collection_info) {
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Null collection_info");
  }
  auto buffer_collection_info =
      reinterpret_cast<magma_sysmem::PlatformBufferDescription*>(collection_info);
  *has_format_modifier_out = buffer_collection_info->has_format_modifier();
  *format_modifier_out = buffer_collection_info->format_modifier();
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_get_buffer_color_space(magma_buffer_format_description_t description,
                                            uint32_t* color_space_out) {
  return magma_collection_info_get_color_space(description, color_space_out);
}

magma_status_t magma_collection_info_get_color_space(magma_collection_info_t collection_info,
                                                     uint32_t* color_space_out) {
  auto buffer_collection_info =
      reinterpret_cast<magma_sysmem::PlatformBufferDescription*>(collection_info);
  auto result = buffer_collection_info->GetColorSpace(color_space_out);
  return DRET(result ? MAGMA_STATUS_OK : MAGMA_STATUS_INVALID_ARGS);
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_get_buffer_coherency_domain(magma_buffer_format_description_t description,
                                                 uint32_t* coherency_domain_out) {
  return magma_collection_info_get_coherency_domain(description, coherency_domain_out);
}

magma_status_t magma_collection_info_get_coherency_domain(magma_collection_info_t collection_info,
                                                          uint32_t* coherency_domain_out) {
  if (!collection_info) {
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Null collection_info");
  }
  auto buffer_collection_info =
      reinterpret_cast<magma_sysmem::PlatformBufferDescription*>(collection_info);
  *coherency_domain_out = buffer_collection_info->coherency_domain();
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_get_buffer_count(magma_buffer_format_description_t description,
                                      uint32_t* count_out) {
  return magma_collection_info_get_buffer_count(description, count_out);
}
magma_status_t magma_collection_info_get_buffer_count(magma_collection_info_t collection_info,
                                                      uint32_t* count_out) {
  if (!collection_info) {
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Null collection_info");
  }
  auto buffer_collection_info =
      reinterpret_cast<magma_sysmem::PlatformBufferDescription*>(collection_info);
  *count_out = buffer_collection_info->count();
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_get_buffer_is_secure(magma_buffer_format_description_t description,
                                          magma_bool_t* is_secure_out) {
  return magma_collection_info_get_is_secure(description, is_secure_out);
}

magma_status_t magma_collection_info_get_is_secure(magma_collection_info_t collection_info,
                                                   magma_bool_t* is_secure_out) {
  if (!collection_info) {
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Null collection_info");
  }
  auto buffer_collection_info =
      reinterpret_cast<magma_sysmem::PlatformBufferDescription*>(collection_info);
  *is_secure_out = buffer_collection_info->is_secure();
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_buffer_collection_import(magma_sysmem_connection_t connection,
                                              magma_handle_t handle,
                                              magma_buffer_collection_t* collection_out) {
  return magma_sysmem_connection_import_buffer_collection(connection, handle, collection_out);
}

magma_status_t magma_sysmem_connection_import_buffer_collection(
    magma_sysmem_connection_t connection, magma_handle_t handle,
    magma_buffer_collection_t* collection_out) {
  auto sysmem_connection = reinterpret_cast<magma_sysmem::PlatformSysmemConnection*>(connection);
  if (!handle) {
    magma::Status status = sysmem_connection->CreateBufferCollectionToken(&handle);
    if (!status.ok()) {
      return DRET(status.get());
    }
  }
  std::unique_ptr<magma_sysmem::PlatformBufferCollection> buffer_collection;
  magma::Status status = sysmem_connection->ImportBufferCollection(handle, &buffer_collection);
  if (!status.ok())
    return status.get();
  *collection_out = reinterpret_cast<magma_buffer_collection_t>(buffer_collection.release());
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
void magma_buffer_collection_release(magma_sysmem_connection_t connection,
                                     magma_buffer_collection_t collection) {
  magma_buffer_collection_release2(collection);
}

void magma_buffer_collection_release2(magma_buffer_collection_t collection) {
  delete reinterpret_cast<magma_sysmem::PlatformBufferCollection*>(collection);
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_buffer_constraints_create(
    magma_sysmem_connection_t connection,
    const magma_buffer_format_constraints_t* buffer_constraints_in,
    magma_sysmem_buffer_constraints_t* constraints_out) {
  return magma_sysmem_connection_create_buffer_constraints(connection, buffer_constraints_in,
                                                           constraints_out);
}

magma_status_t magma_sysmem_connection_create_buffer_constraints(
    magma_sysmem_connection_t connection,
    const magma_buffer_format_constraints_t* buffer_constraints_in,
    magma_sysmem_buffer_constraints_t* constraints_out) {
  auto sysmem_connection = reinterpret_cast<magma_sysmem::PlatformSysmemConnection*>(connection);
  std::unique_ptr<magma_sysmem::PlatformBufferConstraints> buffer_constraints;
  magma::Status status =
      sysmem_connection->CreateBufferConstraints(buffer_constraints_in, &buffer_constraints);
  if (!status.ok())
    return status.get();
  *constraints_out =
      reinterpret_cast<magma_sysmem_buffer_constraints_t>(buffer_constraints.release());
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_buffer_constraints_add_additional(
    magma_sysmem_connection_t connection, magma_sysmem_buffer_constraints_t constraints,
    const magma_buffer_format_additional_constraints_t* additional) {
  auto buffer_constraints = reinterpret_cast<magma_sysmem::PlatformBufferConstraints*>(constraints);
  return buffer_constraints->AddAdditionalConstraints(additional).get();
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_buffer_constraints_set_format(
    magma_sysmem_connection_t connection, magma_sysmem_buffer_constraints_t constraints,
    uint32_t index, const magma_image_format_constraints_t* format_constraints) {
  return magma_buffer_constraints_set_format2(constraints, index, format_constraints);
}

magma_status_t magma_buffer_constraints_set_format2(
    magma_sysmem_buffer_constraints_t constraints, uint32_t index,
    const magma_image_format_constraints_t* format_constraints) {
  auto buffer_constraints = reinterpret_cast<magma_sysmem::PlatformBufferConstraints*>(constraints);
  return buffer_constraints->SetImageFormatConstraints(index, format_constraints).get();
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_buffer_constraints_set_colorspaces(
    magma_sysmem_connection_t connection, magma_sysmem_buffer_constraints_t constraints,
    uint32_t index, uint32_t color_space_count, const uint32_t* color_spaces) {
  return magma_buffer_constraints_set_colorspaces2(constraints, index, color_space_count,
                                                   color_spaces);
}

magma_status_t magma_buffer_constraints_set_colorspaces2(
    magma_sysmem_buffer_constraints_t constraints, uint32_t index, uint32_t color_space_count,
    const uint32_t* color_spaces) {
  auto buffer_constraints = reinterpret_cast<magma_sysmem::PlatformBufferConstraints*>(constraints);
  return buffer_constraints->SetColorSpaces(index, color_space_count, color_spaces).get();
}

// TODO(fxbug.dev/120286): Remove
void magma_buffer_constraints_release(magma_sysmem_connection_t connection,
                                      magma_sysmem_buffer_constraints_t constraints) {
  return magma_buffer_constraints_release2(constraints);
}
void magma_buffer_constraints_release2(magma_sysmem_buffer_constraints_t constraints) {
  delete reinterpret_cast<magma_sysmem::PlatformBufferConstraints*>(constraints);
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_buffer_collection_set_constraints(
    magma_sysmem_connection_t connection, magma_buffer_collection_t collection,
    magma_sysmem_buffer_constraints_t constraints) {
  return magma_buffer_collection_set_constraints2(collection, constraints);
}

magma_status_t magma_buffer_collection_set_constraints2(
    magma_buffer_collection_t collection, magma_sysmem_buffer_constraints_t constraints) {
  auto buffer_collection = reinterpret_cast<magma_sysmem::PlatformBufferCollection*>(collection);
  auto buffer_constraints = reinterpret_cast<magma_sysmem::PlatformBufferConstraints*>(constraints);
  return buffer_collection->SetConstraints(buffer_constraints).get();
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_sysmem_get_description_from_collection(
    magma_sysmem_connection_t connection, magma_buffer_collection_t collection,
    magma_buffer_format_description_t* buffer_format_description_out) {
  return magma_buffer_collection_get_collection_info(collection, buffer_format_description_out);
}

magma_status_t magma_buffer_collection_get_collection_info(
    magma_buffer_collection_t collection, magma_collection_info_t* collection_info_out) {
  auto buffer_collection = reinterpret_cast<magma_sysmem::PlatformBufferCollection*>(collection);
  std::unique_ptr<magma_sysmem::PlatformBufferDescription> collection_info;
  magma::Status status = buffer_collection->GetBufferDescription(&collection_info);
  if (!status.ok()) {
    return DRET_MSG(status.get(), "GetBufferDescription failed");
  }

  *collection_info_out = reinterpret_cast<magma_collection_info_t>(collection_info.release());
  return MAGMA_STATUS_OK;
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_sysmem_get_buffer_handle_from_collection(magma_sysmem_connection_t connection,
                                                              magma_buffer_collection_t collection,
                                                              uint32_t index,
                                                              magma_handle_t* buffer_handle_out,
                                                              uint32_t* vmo_offset_out) {
  return magma_buffer_collection_get_buffer_handle(collection, index, buffer_handle_out,
                                                   vmo_offset_out);
}

magma_status_t magma_buffer_collection_get_buffer_handle(magma_buffer_collection_t collection,
                                                         uint32_t index,
                                                         magma_handle_t* buffer_handle_out,
                                                         uint32_t* vmo_offset_out) {
  auto buffer_collection = reinterpret_cast<magma_sysmem::PlatformBufferCollection*>(collection);
  return buffer_collection->GetBufferHandle(index, buffer_handle_out, vmo_offset_out).get();
}

// TODO(fxbug.dev/120286): Remove
magma_status_t magma_get_description_format_index(magma_sysmem_connection_t connection,
                                                  magma_buffer_format_description_t description,
                                                  magma_sysmem_buffer_constraints_t constraints,
                                                  magma_bool_t* format_valid_out,
                                                  uint32_t format_valid_count) {
  return magma_collection_info_get_format_index(description, constraints, format_valid_out,
                                                format_valid_count);
}

magma_status_t magma_collection_info_get_format_index(magma_collection_info_t collection_info,
                                                      magma_sysmem_buffer_constraints_t constraints,
                                                      magma_bool_t* format_valid_out,
                                                      uint32_t format_valid_count) {
  auto buffer_collection_info =
      reinterpret_cast<magma_sysmem::PlatformBufferDescription*>(collection_info);
  auto buffer_constraints = reinterpret_cast<magma_sysmem::PlatformBufferConstraints*>(constraints);
  if (!buffer_collection_info->GetFormatIndex(buffer_constraints, format_valid_out,
                                              format_valid_count))
    return DRET(MAGMA_STATUS_INTERNAL_ERROR);
  return MAGMA_STATUS_OK;
}
