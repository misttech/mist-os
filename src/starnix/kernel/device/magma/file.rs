// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use fuchsia_zircon as zx;
use fuchsia_zircon::HandleBased;
use magma::*;
use std::{collections::HashMap, sync::Arc};

use super::{ffi::*, magma::*};
use crate::{
    device::wayland::image_file::*,
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        *,
    },
    lock::Mutex,
    logging::{impossible_error, log_error, log_warn},
    mm::MemoryAccessorExt,
    syscalls::*,
    task::CurrentTask,
    types::*,
};

#[derive(Clone)]
pub enum BufferInfo {
    Default,
    Image(ImageInfo),
}

/// A `MagmaConnection` is an internal representation of a `magma_connection_t`.
type MagmaConnection = u64;

/// A `MagmaDevice` is an RAII wrapper around a `magma_device_t`.
pub struct MagmaDevice {
    pub handle: magma_device_t,
}

impl Drop for MagmaDevice {
    /// SAFETY: Makes an FFI call to release a handle that was imported using `magma_device_import`.
    fn drop(&mut self) {
        unsafe { magma_device_release(self.handle) }
    }
}

/// A `BufferMap` stores all the magma buffers for a given connection.
type BufferMap = HashMap<magma_buffer_t, BufferInfo>;

/// A `ConnectionMap` stores the `BufferMap`s associated with each magma connection.
pub type ConnectionMap = HashMap<MagmaConnection, BufferMap>;

pub type DeviceMap = HashMap<magma_device_t, MagmaDevice>;

pub struct MagmaFile {
    devices: Arc<Mutex<DeviceMap>>,
    connections: Arc<Mutex<ConnectionMap>>,
}

impl MagmaFile {
    pub fn new_file(
        _current_task: &CurrentTask,
        _dev: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(Self {
            devices: Arc::new(Mutex::new(HashMap::new())),
            connections: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    /// Returns a duplicate of the VMO associated with the file at `fd`, as well as a `BufferInfo`
    /// of the correct type for that file.
    ///
    /// Returns an error if the file does not contain a buffer.
    fn get_vmo_and_magma_buffer(
        current_task: &CurrentTask,
        fd: FdNumber,
    ) -> Result<(zx::Vmo, BufferInfo), Errno> {
        let file = current_task.files.get(fd)?;
        if let Some(file) = file.downcast_file::<ImageFile>() {
            let buffer = BufferInfo::Image(file.info.clone());
            Ok((
                file.vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(impossible_error)?,
                buffer,
            ))
        } else if let Some(file) = file.downcast_file::<VmoFileObject>() {
            let buffer = BufferInfo::Default;
            Ok((
                file.vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(impossible_error)?,
                buffer,
            ))
        } else {
            error!(EINVAL)
        }
    }

    /// Adds a `BufferInfo` for the given `magma_buffer_t`, associated with the specified
    /// connection.
    fn add_buffer_info(
        &self,
        connection: magma_connection_t,
        buffer: magma_buffer_t,
        buffer_info: BufferInfo,
    ) {
        self.connections
            .lock()
            .entry(connection)
            .or_insert_with(HashMap::new)
            .insert(buffer, buffer_info);
    }

    /// Returns a `BufferInfo` for the given `magma_buffer_t`, if one exists for the given
    /// `connection`. Otherwise returns `None`.
    fn get_buffer_info(
        &self,
        connection: magma_connection_t,
        buffer: magma_buffer_t,
    ) -> Option<BufferInfo> {
        match self.connections.lock().get(&connection) {
            Some(buffers) => buffers.get(&buffer).cloned(),
            _ => None,
        }
    }
}

impl FileOps for MagmaFile {
    fileops_impl_nonseekable!();

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _request: u32,
        user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        let (command, command_type) = read_magma_command_and_type(current_task, user_addr)?;
        let response_address = UserAddress::from(command.response_address);

        match command_type {
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_IMPORT => {
                let (control, mut response) = read_control_and_response(current_task, &command)?;
                let device = device_import(control, &mut response)?;
                (*self.devices.lock()).insert(device.handle, device);

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_CREATE_CONNECTION => {
                let (control, mut response): (
                    virtio_magma_device_create_connection_ctrl,
                    virtio_magma_device_create_connection_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                create_connection(control, &mut response);

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE => {
                let (control, mut response): (
                    virtio_magma_connection_release_ctrl_t,
                    virtio_magma_connection_release_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                release_connection(control, &mut response, &mut self.connections.lock());

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_RELEASE => {
                let (control, mut response): (
                    virtio_magma_device_release_ctrl_t,
                    virtio_magma_device_release_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                device_release(control, &mut response, &mut self.devices.lock());

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_VIRT_CONNECTION_CREATE_IMAGE => {
                let (control, mut response): (
                    virtio_magma_virt_connection_create_image_ctrl_t,
                    virtio_magma_virt_connection_create_image_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                if let Ok(buffer) = create_image(current_task, control, &mut response) {
                    self.add_buffer_info(control.connection, response.image_out, buffer);
                }

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_VIRT_CONNECTION_GET_IMAGE_INFO => {
                let (control, mut response): (
                    virtio_magma_virt_connection_get_image_info_ctrl_t,
                    virtio_magma_virt_connection_get_image_info_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let image_info_address_ref =
                    UserRef::new(UserAddress::from(control.image_info_out));
                let image_info_ptr = current_task.mm.read_object(image_info_address_ref)?;

                match self.get_buffer_info(
                    control.connection as magma_connection_t,
                    control.image as magma_buffer_t,
                ) {
                    Some(BufferInfo::Image(image_info)) => {
                        let image_info_ref = UserRef::new(image_info_ptr);
                        current_task.mm.write_object(image_info_ref, &image_info.info)?;
                        response.result_return = MAGMA_STATUS_OK as u64;
                    }
                    _ => {
                        log_error!("No image info was found for buffer: {:?}", { control.image });
                        response.result_return = MAGMA_STATUS_INVALID_ARGS as u64;
                    }
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_VIRT_CONNECTION_GET_IMAGE_INFO as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_FLUSH => {
                let (control, mut response): (
                    virtio_magma_connection_flush_ctrl_t,
                    virtio_magma_connection_flush_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                flush(control, &mut response);

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_READ_NOTIFICATION_CHANNEL => {
                let (control, mut response): (
                    virtio_magma_connection_read_notification_channel_ctrl_t,
                    virtio_magma_connection_read_notification_channel_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                read_notification_channel(current_task, control, &mut response)?;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_GET_HANDLE => {
                let (control, mut response): (
                    virtio_magma_buffer_get_handle_ctrl_t,
                    virtio_magma_buffer_get_handle_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                get_buffer_handle(current_task, control, &mut response)?;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_release_buffer_ctrl_t,
                    virtio_magma_connection_release_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                if let Some(buffers) = self.connections.lock().get_mut(&{ control.connection }) {
                    match buffers.remove(&{ control.buffer }) {
                        Some(_) => release_buffer(control, &mut response),
                        _ => {
                            log_error!("Calling magma_release_buffer with an invalid buffer.");
                        }
                    };
                }

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_EXPORT => {
                let (control, mut response): (
                    virtio_magma_buffer_export_ctrl_t,
                    virtio_magma_buffer_export_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                export_buffer(current_task, control, &mut response, &self.connections.lock())?;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_IMPORT_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_import_buffer_ctrl_t,
                    virtio_magma_connection_import_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let buffer_fd = FdNumber::from_raw(control.buffer_handle as i32);
                let (vmo, buffer) = MagmaFile::get_vmo_and_magma_buffer(current_task, buffer_fd)?;

                let mut buffer_out = magma_buffer_t::default();
                let mut size_out = 0u64;
                let mut id_out = magma_buffer_id_t::default();
                response.result_return = unsafe {
                    magma_connection_import_buffer(
                        control.connection as magma_connection_t,
                        vmo.into_raw(),
                        &mut size_out,
                        &mut buffer_out,
                        &mut id_out,
                    ) as u64
                };

                // Store the information for the newly imported buffer.
                self.add_buffer_info(control.connection as magma_connection_t, buffer_out, buffer);
                // Import is expected to close the file that was imported.
                let _ = current_task.files.close(buffer_fd);

                response.buffer_out = buffer_out;
                response.size_out = size_out;
                response.id_out = id_out;
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_IMPORT_BUFFER as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_GET_NOTIFICATION_CHANNEL_HANDLE => {
                let (control, mut response): (
                    virtio_magma_connection_get_notification_channel_handle_ctrl_t,
                    virtio_magma_connection_get_notification_channel_handle_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                response.result_return = unsafe {
                    magma_connection_get_notification_channel_handle(
                        control.connection as magma_connection_t,
                    )
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_GET_NOTIFICATION_CHANNEL_HANDLE as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_CREATE_CONTEXT => {
                let (control, mut response): (
                    virtio_magma_connection_create_context_ctrl_t,
                    virtio_magma_connection_create_context_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let mut context_id_out = 0;
                response.result_return = unsafe {
                    magma_connection_create_context(
                        control.connection as magma_connection_t,
                        &mut context_id_out,
                    ) as u64
                };
                response.context_id_out = context_id_out as u64;

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_CREATE_CONTEXT as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE_CONTEXT => {
                let (control, mut response): (
                    virtio_magma_connection_release_context_ctrl_t,
                    virtio_magma_connection_release_context_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                unsafe {
                    magma_connection_release_context(
                        control.connection as magma_connection_t,
                        control.context_id,
                    );
                }

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_RELEASE_CONTEXT as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_CREATE_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_create_buffer_ctrl_t,
                    virtio_magma_connection_create_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let mut size_out = 0;
                let mut buffer_out = 0;
                let mut id_out = 0;
                response.result_return = unsafe {
                    magma_connection_create_buffer(
                        control.connection as magma_connection_t,
                        control.size,
                        &mut size_out,
                        &mut buffer_out,
                        &mut id_out,
                    ) as u64
                };
                response.size_out = size_out;
                response.buffer_out = buffer_out;
                response.id_out = id_out;
                self.add_buffer_info(
                    control.connection as magma_connection_t,
                    buffer_out,
                    BufferInfo::Default,
                );

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_CREATE_BUFFER as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_CREATE_SEMAPHORE => {
                let (control, mut response): (
                    virtio_magma_connection_create_semaphore_ctrl_t,
                    virtio_magma_connection_create_semaphore_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let mut semaphore_out = 0;
                let mut semaphore_id = 0;
                response.result_return = unsafe {
                    magma_connection_create_semaphore(
                        control.connection as magma_connection_t,
                        &mut semaphore_out,
                        &mut semaphore_id,
                    ) as u64
                };

                response.semaphore_out = semaphore_out;
                response.id_out = semaphore_id;
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_CREATE_SEMAPHORE as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_GET_ERROR => {
                let (control, mut response): (
                    virtio_magma_connection_get_error_ctrl_t,
                    virtio_magma_connection_get_error_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                response.result_return = unsafe {
                    magma_connection_get_error(control.connection as magma_connection_t) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_GET_ERROR as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_IMPORT_SEMAPHORE => {
                let (control, mut response): (
                    virtio_magma_connection_import_semaphore_ctrl_t,
                    virtio_magma_connection_import_semaphore_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let mut semaphore_out = 0;
                let mut semaphore_id = 0;
                response.result_return = unsafe {
                    magma_connection_import_semaphore(
                        control.connection as magma_connection_t,
                        control.semaphore_handle,
                        &mut semaphore_out,
                        &mut semaphore_id,
                    ) as u64
                };
                response.semaphore_out = semaphore_out;
                response.id_out = semaphore_id;

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_IMPORT_SEMAPHORE as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE_SEMAPHORE => {
                let (control, mut response): (
                    virtio_magma_connection_release_semaphore_ctrl_t,
                    virtio_magma_connection_release_semaphore_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                unsafe {
                    magma_connection_release_semaphore(
                        control.connection as magma_connection_t,
                        control.semaphore as magma_semaphore_t,
                    );
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_RELEASE_SEMAPHORE as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_SEMAPHORE_EXPORT => {
                let (control, mut response): (
                    virtio_magma_semaphore_export_ctrl_t,
                    virtio_magma_semaphore_export_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let mut semaphore_handle_out = 0;
                response.result_return = unsafe {
                    magma_semaphore_export(
                        control.semaphore as magma_semaphore_t,
                        &mut semaphore_handle_out,
                    ) as u64
                };
                response.semaphore_handle_out = semaphore_handle_out as u64;

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_SEMAPHORE_EXPORT as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_SEMAPHORE_RESET => {
                let (control, mut response): (
                    virtio_magma_semaphore_reset_ctrl_t,
                    virtio_magma_semaphore_reset_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                unsafe {
                    magma_semaphore_reset(control.semaphore as magma_semaphore_t);
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_SEMAPHORE_RESET as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_SEMAPHORE_SIGNAL => {
                let (control, mut response): (
                    virtio_magma_semaphore_signal_ctrl_t,
                    virtio_magma_semaphore_signal_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                unsafe {
                    magma_semaphore_signal(control.semaphore as magma_semaphore_t);
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_SEMAPHORE_SIGNAL as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_MAP_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_map_buffer_ctrl_t,
                    virtio_magma_connection_map_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                response.result_return = unsafe {
                    magma_connection_map_buffer(
                        control.connection as magma_connection_t,
                        control.hw_va,
                        control.buffer,
                        control.offset,
                        control.length,
                        control.map_flags,
                    ) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_MAP_BUFFER as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_POLL => {
                let (control, mut response): (virtio_magma_poll_ctrl_t, virtio_magma_poll_resp_t) =
                    read_control_and_response(current_task, &command)?;

                let num_items = control.count as usize / std::mem::size_of::<StarnixPollItem>();
                let items_ref = UserRef::new(UserAddress::from(control.items));
                // Read the poll items as `StarnixPollItem`, since they contain a union. Also note
                // that the minimum length of the vector is 1, to always have a valid reference for
                // `magma_poll`.
                let mut starnix_items =
                    vec![StarnixPollItem::default(); std::cmp::max(num_items, 1)];
                current_task.mm.read_objects(items_ref, &mut starnix_items)?;
                // Then convert each item "manually" into `magma_poll_item_t`.
                let mut magma_items: Vec<magma_poll_item_t> =
                    starnix_items.iter().map(|item| item.as_poll_item()).collect();

                response.result_return = unsafe {
                    magma_poll(
                        &mut magma_items[0] as *mut magma_poll_item,
                        num_items as u32,
                        control.timeout_ns,
                    ) as u64
                };

                // Convert the poll items back to a serializable version after the `magma_poll`
                // call.
                let starnix_items: Vec<StarnixPollItem> =
                    magma_items.iter().map(StarnixPollItem::new).collect();
                current_task.mm.write_objects(items_ref, &starnix_items)?;

                response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_POLL as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_EXECUTE_COMMAND => {
                let (control, mut response): (
                    virtio_magma_connection_execute_command_ctrl_t,
                    virtio_magma_connection_execute_command_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                execute_command(current_task, control, &mut response)?;

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_EXECUTE_COMMAND as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_EXECUTE_IMMEDIATE_COMMANDS => {
                let (control, mut response): (
                    virtio_magma_connection_execute_immediate_commands_ctrl_t,
                    virtio_magma_connection_execute_immediate_commands_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let status = execute_immediate_commands(current_task, control)?;

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_EXECUTE_IMMEDIATE_COMMANDS
                        as u32;
                response.result_return = status as u64;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_QUERY => {
                let (control, mut response): (
                    virtio_magma_device_query_ctrl_t,
                    virtio_magma_device_query_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                query(current_task, control, &mut response)?;

                response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_DEVICE_QUERY as u32;
                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_UNMAP_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_unmap_buffer_ctrl_t,
                    virtio_magma_connection_unmap_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                unsafe {
                    magma_connection_unmap_buffer(
                        control.connection as magma_connection_t,
                        control.hw_va,
                        control.buffer,
                    )
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_UNMAP_BUFFER as u32;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_PERFORM_BUFFER_OP => {
                let (control, mut response): (
                    virtio_magma_connection_perform_buffer_op_ctrl_t,
                    virtio_magma_connection_perform_buffer_op_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                response.result_return = unsafe {
                    magma_connection_perform_buffer_op(
                        control.connection as magma_connection_t,
                        control.buffer,
                        control.options,
                        control.start_offset,
                        control.length,
                    ) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_PERFORM_BUFFER_OP as u32;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_GET_INFO => {
                let (control, mut response): (
                    virtio_magma_buffer_get_info_ctrl_t,
                    virtio_magma_buffer_get_info_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let mut buffer_info = magma_buffer_info_t { committed_byte_count: 0, size: 0 };

                let status = unsafe { magma_buffer_get_info(control.buffer, &mut buffer_info) };

                if status == MAGMA_STATUS_OK {
                    current_task.mm.write_object(
                        UserRef::<magma_buffer_info_t>::new(UserAddress::from(control.info_out)),
                        &buffer_info,
                    )?;
                }

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_GET_INFO as u32;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_SET_CACHE_POLICY => {
                let (control, mut response): (
                    virtio_magma_buffer_set_cache_policy_ctrl_t,
                    virtio_magma_buffer_set_cache_policy_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                response.result_return = unsafe {
                    magma_buffer_set_cache_policy(
                        control.buffer,
                        control.policy as magma_cache_policy_t,
                    ) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_SET_CACHE_POLICY as u32;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_GET_CACHE_POLICY => {
                let (control, mut response): (
                    virtio_magma_buffer_get_cache_policy_ctrl_t,
                    virtio_magma_buffer_get_cache_policy_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let mut policy: magma_cache_policy_t = MAGMA_CACHE_POLICY_CACHED;

                let status = unsafe { magma_buffer_get_cache_policy(control.buffer, &mut policy) };

                if status == MAGMA_STATUS_OK {
                    response.cache_policy_out = policy as u64;
                }
                response.result_return = status as u64;
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_GET_CACHE_POLICY as u32;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_CLEAN_CACHE => {
                let (control, mut response): (
                    virtio_magma_buffer_clean_cache_ctrl_t,
                    virtio_magma_buffer_clean_cache_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                response.result_return = unsafe {
                    magma_buffer_clean_cache(
                        control.buffer,
                        control.offset,
                        control.size,
                        control.operation as magma_cache_operation_t,
                    ) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_CLEAN_CACHE as u32;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_SET_NAME => {
                let (control, mut response): (
                    virtio_magma_buffer_set_name_ctrl_t,
                    virtio_magma_buffer_set_name_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let wrapper_ref = UserRef::<virtmagma_buffer_set_name_wrapper>::new(
                    UserAddress::from(control.name),
                );
                let wrapper = current_task.mm.read_object(wrapper_ref)?;

                let name = current_task.mm.read_buffer(&UserBuffer {
                    address: UserAddress::from(wrapper.name_address),
                    length: wrapper.name_size as usize, // name_size includes null terminate byte
                })?;

                response.result_return = unsafe {
                    #[cfg(target_arch = "x86_64")]
                    let name_ptr = &name[0] as *const u8 as *const i8;
                    #[cfg(target_arch = "aarch64")]
                    let name_ptr = &name[0] as *const u8 as *const u8;
                    magma_buffer_set_name(control.buffer, name_ptr) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_SET_NAME as u32;

                current_task.mm.write_object(UserRef::new(response_address), &response)
            }
            t => {
                log_warn!("Got unknown request: {:?}", t);
                error!(ENOSYS)
            }
        }?;

        Ok(SUCCESS)
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EINVAL)
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EINVAL)
    }
}
