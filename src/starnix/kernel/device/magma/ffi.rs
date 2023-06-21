// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use fuchsia_zircon::{self as zx, AsHandleRef, HandleBased};
use magma::*;
use std::mem::ManuallyDrop;
use zerocopy::{AsBytes, FromBytes, FromZeroes};

use crate::{
    device::{
        magma::{
            file::{
                BufferInfo, ConnectionInfo, ConnectionMap, DeviceMap, MagmaBuffer, MagmaConnection,
                MagmaDevice,
            },
            magma::create_drm_image,
        },
        wayland::image_file::{ImageFile, ImageInfo},
    },
    fs::{Anon, FdFlags, FsNodeInfo, VmoFileObject},
    mm::{MemoryAccessor, MemoryAccessorExt},
    task::CurrentTask,
    types::*,
};

/// Reads a sequence of objects starting at `addr`, ensuring at least one element is in the returned
/// Vec.
///
/// # Parameters
///   - `current_task`: The task from which to read the objects.
///   - `addr`: The address of the first item to read.
///   - `item_count`: The number of items to read. If 0, a 1-item vector will be returned to make
///                   sure that the calling code can safely pass `&mut vec[0]` to libmagma.
fn read_objects<T>(
    current_task: &CurrentTask,
    addr: UserAddress,
    item_count: usize,
) -> Result<Vec<T>, Errno>
where
    T: Default + Clone + FromBytes,
{
    Ok(if item_count > 0 {
        let user_ref: UserRef<T> = addr.into();
        current_task.mm.read_objects_to_vec(user_ref, item_count)?
    } else {
        vec![T::default()]
    })
}

/// Creates a connection for a given device.
///
/// # Parameters
///   - `control`: The control struct containing the device to create a connection to.
///   - `response`: The struct that will be filled out to contain the response. This struct can be
///                 written back to userspace.
///
/// SAFETY: Makes an FFI call to populate the fields of `response`.
pub fn create_connection(
    control: virtio_magma_device_create_connection_ctrl,
    response: &mut virtio_magma_device_create_connection_resp_t,
    connections: &mut ConnectionMap,
) {
    let mut connection_out: magma_connection_t = 0;
    response.result_return =
        unsafe { magma_device_create_connection(control.device, &mut connection_out) as u64 };

    response.connection_out = connection_out;
    response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_DEVICE_CREATE_CONNECTION as u32;
    if response.result_return as i32 == MAGMA_STATUS_OK {
        connections.insert(
            response.connection_out,
            ConnectionInfo::new(Arc::new(MagmaConnection { handle: response.connection_out })),
        );
    }
}

/// Creates a DRM image VMO and imports it to magma.
///
/// Returns a `BufferInfo` containing the associated `BufferCollectionImportToken` and the magma
/// image info.
///
/// Upon successful completion, `response.image_out` will contain the handle to the magma buffer.
///
/// SAFETY: Makes an FFI call which takes ownership of a raw VMO handle. Invalid parameters are
/// dealt with by magma.
pub fn create_image(
    current_task: &CurrentTask,
    control: virtio_magma_virt_connection_create_image_ctrl_t,
    response: &mut virtio_magma_virt_connection_create_image_resp_t,
    connection: &Arc<MagmaConnection>,
) -> Result<BufferInfo, Errno> {
    let create_info_address = UserAddress::from(control.create_info);
    let create_info_ptr: u64 = current_task.mm.read_object(UserRef::new(create_info_address))?;

    let create_info_address = UserAddress::from(create_info_ptr);
    let create_info = current_task.mm.read_object(UserRef::new(create_info_address))?;

    response.hdr.type_ =
        virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_VIRT_CONNECTION_CREATE_IMAGE as u32;
    response.result_return = MAGMA_STATUS_INVALID_ARGS as u64;
    response.image_out = 0;
    response.buffer_id_out = 0;
    response.size_out = 0;

    let (vmo, token, info) = create_drm_image(0, &create_info).map_err(|status| {
        response.result_return = status as u64;
        errno!(EINVAL)
    })?;

    let mut buffer_out = magma_buffer_t::default();
    let mut buffer_id_out = magma_buffer_id_t::default();
    let mut size_out = 0u64;
    response.result_return = unsafe {
        magma_connection_import_buffer(
            connection.handle,
            vmo.into_raw(),
            &mut size_out,
            &mut buffer_out,
            &mut buffer_id_out,
        ) as u64
    };

    response.image_out = buffer_out;
    response.buffer_id_out = buffer_id_out;
    response.size_out = size_out;

    Ok(BufferInfo::Image(ImageInfo { info, token }))
}

/// Attempts to open a device at a path. Fails if the device is not a supported one.
///
/// # Parameters
///   - 'path': The filesystem path to open.
///
/// SAFETY: Makes FFI calls to import and query devices.
fn attempt_open_path(path: std::path::PathBuf) -> Result<MagmaDevice, Errno> {
    let path = path.into_os_string().into_string().map_err(|_| errno!(EINVAL))?;
    let (client_channel, server_channel) = zx::Channel::create();

    fdio::service_connect(&path, server_channel).map_err(|_| errno!(EINVAL))?;
    // `magma_device_import` takes ownership of the channel, so don't drop it again.
    let client_channel = ManuallyDrop::new(client_channel);
    let device_channel = client_channel.raw_handle();

    let mut device_out: u64 = 0;
    let result = unsafe { magma_device_import(device_channel, &mut device_out as *mut u64) };

    if result != MAGMA_STATUS_OK {
        return Err(errno!(EINVAL));
    }
    let magma_device = MagmaDevice { handle: device_out };

    let mut result_out = 0;
    let mut result_buffer_out = 0;
    let query_result = unsafe {
        magma_device_query(
            device_out,
            MAGMA_QUERY_VENDOR_ID,
            &mut result_buffer_out,
            &mut result_out,
        )
    };
    if query_result != MAGMA_STATUS_OK {
        return Err(errno!(EINVAL));
    }

    let supported_gpu_vendors = [MAGMA_VENDOR_ID_MALI as u64, MAGMA_VENDOR_ID_INTEL as u64];

    if !supported_gpu_vendors.contains(&result_out) {
        return Err(errno!(EINVAL));
    }
    Ok(magma_device)
}

/// Imports a device to magma.
///
/// # Parameters
///   - `control`: The control struct containing the device channel to import from.
///   - `response`: The struct that will be filled out to contain the response. This struct can be
///                 written back to userspace.
///
/// SAFETY: Makes an FFI call to populate the fields of `response`.
pub fn device_import(
    _control: virtio_magma_device_import_ctrl_t,
    response: &mut virtio_magma_device_import_resp_t,
) -> Result<MagmaDevice, Errno> {
    // TODO(fxbug.dev/100454): This currently picks the first available device in the allowlist, but
    // multiple devices should probably be exposed to clients.
    let entries =
        std::fs::read_dir("/dev/class/gpu").map_err(|_| errno!(EINVAL))?.filter_map(|x| x.ok());

    let magma_device = entries
        .filter_map(|entry| attempt_open_path(entry.path()).ok())
        .next()
        .ok_or_else(|| errno!(EINVAL))?;
    response.result_return = MAGMA_STATUS_OK as u64;

    response.device_out = magma_device.handle;
    response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_DEVICE_IMPORT as u32;

    Ok(magma_device)
}

/// Releases a magma device.
///
/// # Parameters
///  - `control`: The control message that contains the device to release.
///  - `response`: The response message that will be updated to write back to user space.
pub fn device_release(
    control: virtio_magma_device_release_ctrl_t,
    response: &mut virtio_magma_device_release_resp_t,
    devices: &mut DeviceMap,
) {
    let device = control.device as magma_device_t;
    // Dropping the device will call magma_device_release.
    devices.remove(&device);
    response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_DEVICE_RELEASE as u32;
}

/// `WireDescriptor` matches the struct used by libmagma_virt to encode some fields of the magma
/// command descriptor.
#[repr(C)]
#[derive(FromZeroes, FromBytes, AsBytes, Default, Debug)]
struct WireDescriptor {
    resource_count: u32,
    command_buffer_count: u32,
    wait_semaphore_count: u32,
    signal_semaphore_count: u32,
    flags: u64,
}

/// Executes a magma command.
///
/// This function bridges between the virtmagma structs and the magma structures. It also copies the
/// data into starnix in order to be able to pass pointers to the resources, command buffers, and
/// semaphore ids to magma.
///
/// SAFETY: Makes an FFI call to populate the fields of `response`.
pub fn execute_command(
    current_task: &CurrentTask,
    control: virtio_magma_connection_execute_command_ctrl_t,
    response: &mut virtio_magma_connection_execute_command_resp_t,
    connection: &Arc<MagmaConnection>,
) -> Result<(), Errno> {
    let virtmagma_command_descriptor_addr =
        UserRef::<virtmagma_command_descriptor>::new(control.descriptor.into());
    let command_descriptor = current_task.mm.read_object(virtmagma_command_descriptor_addr)?;

    // Read the virtmagma-internal struct that contains the counts and flags for the magma command
    // descriptor.
    let wire_descriptor: WireDescriptor =
        current_task.mm.read_object(UserAddress::from(command_descriptor.descriptor).into())?;

    // This is the command descriptor that will be populated from the virtmagma
    // descriptor and subsequently passed to magma_execute_command.
    let mut magma_command_descriptor = magma_command_descriptor {
        resource_count: wire_descriptor.resource_count,
        command_buffer_count: wire_descriptor.command_buffer_count,
        wait_semaphore_count: wire_descriptor.wait_semaphore_count,
        signal_semaphore_count: wire_descriptor.signal_semaphore_count,
        flags: wire_descriptor.flags,
        ..Default::default()
    };
    let semaphore_count =
        (wire_descriptor.wait_semaphore_count + wire_descriptor.signal_semaphore_count) as usize;

    // Read all the passed in resources, commands, and semaphore ids.
    let mut resources: Vec<magma_exec_resource> = read_objects(
        current_task,
        command_descriptor.resources.into(),
        wire_descriptor.resource_count as usize,
    )?;
    let mut command_buffers: Vec<magma_exec_command_buffer> = read_objects(
        current_task,
        command_descriptor.command_buffers.into(),
        wire_descriptor.command_buffer_count as usize,
    )?;
    let mut semaphores: Vec<u64> =
        read_objects(current_task, command_descriptor.semaphores.into(), semaphore_count)?;

    // Make sure the command descriptor contains valid pointers for the resources, command buffers,
    // and semaphore ids.
    magma_command_descriptor.resources = &mut resources[0] as *mut magma_exec_resource;
    magma_command_descriptor.command_buffers =
        &mut command_buffers[0] as *mut magma_exec_command_buffer;
    magma_command_descriptor.semaphore_ids = &mut semaphores[0] as *mut u64;

    response.result_return = unsafe {
        magma_connection_execute_command(
            connection.handle,
            control.context_id,
            &mut magma_command_descriptor as *mut magma_command_descriptor,
        ) as u64
    };

    Ok(())
}

/// Executes magma immediate commands.
///
/// This function bridges between the virtmagma structs and the magma structures. It also copies the
/// data into starnix in order to be able to pass pointers to the resources, command buffers, and
/// semaphore ids to magma.
///
/// SAFETY: Makes an FFI call to magma_execute_immediate_commands().
pub fn execute_immediate_commands(
    current_task: &CurrentTask,
    control: virtio_magma_connection_execute_immediate_commands_ctrl_t,
    connection: &Arc<MagmaConnection>,
) -> Result<magma_status_t, Errno> {
    let command_buffers_addr = UserAddress::from(control.command_buffers);

    // For virtio-magma, "command_buffers" is an array of virtmagma_command_descriptor instead of
    // magma_inline_command_buffer.
    let descriptors: Vec<virtmagma_command_descriptor> =
        read_objects(current_task, command_buffers_addr, control.command_count as usize)?;

    let mut commands =
        vec![magma_inline_command_buffer::default(); std::cmp::max(descriptors.len(), 1)];

    let mut commands_vec = Vec::<Vec<u8>>::with_capacity(control.command_count as usize);
    let mut semaphore_ids_vec = Vec::<Vec<u64>>::with_capacity(control.command_count as usize);

    for i in 0..control.command_count as usize {
        let size = descriptors[i].command_buffer_size;
        let data = current_task.mm.read_buffer(&UserBuffer {
            address: UserAddress::from(descriptors[i].command_buffers),
            length: size as usize,
        })?;
        commands_vec.push(data);
        commands[i].size = size;

        let semaphore_count =
            (descriptors[i].semaphore_size / core::mem::size_of::<u64>() as u64) as u32;
        commands[i].semaphore_count = semaphore_count;

        let semaphore_ids = read_objects(
            current_task,
            UserAddress::from(descriptors[i].semaphores),
            semaphore_count as usize,
        )?;
        semaphore_ids_vec.push(semaphore_ids);
    }

    let status = unsafe {
        for i in 0..control.command_count as usize {
            commands[i].data = &mut commands_vec[i][0] as *mut u8 as *mut std::ffi::c_void;
            commands[i].semaphore_ids = &mut semaphore_ids_vec[i][0];
        }
        magma_connection_execute_immediate_commands(
            connection.handle,
            control.context_id,
            control.command_count,
            &mut commands[0],
        )
    };

    Ok(status)
}

/// Exports the provided magma buffer into a `zx::Vmo`, which is then wrapped in a file and added
/// to `current_task`'s files.
///
/// The file's `fd` is then written to the response object, which allows the client to interact with
/// the exported buffer.
///
/// # Parameters
///   - `current_task`: The task that is exporting the buffer.
///   - `control`: The control message that contains the buffer to export.
///   - `response`: The response message that will be updated to write back to user space.
///
/// Returns an error if adding the file to `current_task` fails.
///
/// SAFETY: Makes an FFI call to populate the fields of `response`, and creates a `zx::Vmo` from
/// a raw handle provided by magma.
pub fn export_buffer(
    current_task: &CurrentTask,
    _control: virtio_magma_buffer_export_ctrl_t,
    response: &mut virtio_magma_buffer_export_resp_t,
    buffer: &Arc<MagmaBuffer>,
    connections: &ConnectionMap,
) -> Result<(), Errno> {
    let mut buffer_handle_out = 0;
    let status = unsafe {
        magma_buffer_export(
            buffer.handle as magma_buffer_t,
            &mut buffer_handle_out as *mut magma_handle_t,
        )
    };
    if status == MAGMA_STATUS_OK {
        let vmo = unsafe { zx::Vmo::from(zx::Handle::from_raw(buffer_handle_out)) };

        let mut image_info_opt: Option<ImageInfo> = None;
        'outer: for image_map in connections.values() {
            for (image, info) in image_map.buffer_map.iter() {
                if *image == buffer.handle {
                    if let BufferInfo::Image(image_info) = info.clone() {
                        image_info_opt = Some(image_info);
                        break 'outer;
                    }
                }
            }
        }

        let file = {
            if let Some(image_info) = image_info_opt {
                ImageFile::new_file(current_task, image_info, vmo)
            } else {
                Anon::new_file(
                    current_task,
                    Box::new(VmoFileObject::new(Arc::new(vmo))),
                    OpenFlags::RDWR,
                )
            }
        };

        let fd = current_task.add_file(file, FdFlags::empty())?;
        response.buffer_handle_out = fd.raw() as u64;
    }

    response.result_return = status as u64;
    response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_EXPORT as u32;

    Ok(())
}

/// Calls flush on the provided `control.connection`.
///
/// SAFETY: Makes an FFI call to magma, which is expected to handle invalid connection parameters.
pub fn flush(
    _control: virtio_magma_connection_flush_ctrl_t,
    response: &mut virtio_magma_connection_flush_resp_t,
    connection: &Arc<MagmaConnection>,
) {
    response.result_return = unsafe { magma_connection_flush(connection.handle) as u64 };
    response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_FLUSH as u32;
}

/// Fetches a VMO handles from magma wraps it in a file, then adds that file to `current_task`.
///
/// # Parameters
///   - `current_task`: The task that the created file is added to, in `anon_fs`.
///   - `control`: The control message containing the image handle.
///   - `response`: The response which will contain the file descriptor for the created file.
///
/// SAFETY: Makes an FFI call to fetch a VMO handle. The VMO handle is expected to be valid if the
/// FFI call succeeds. Either way, creating a `zx::Vmo` with an invalid handle is safe.
pub fn get_buffer_handle(
    current_task: &CurrentTask,
    _control: virtio_magma_buffer_get_handle_ctrl_t,
    response: &mut virtio_magma_buffer_get_handle_resp_t,
    buffer: &Arc<MagmaBuffer>,
) -> Result<(), Errno> {
    let mut buffer_handle_out = 0;
    let status = unsafe {
        magma_buffer_get_handle(
            buffer.handle as magma_buffer_t,
            &mut buffer_handle_out as *mut magma_handle_t,
        )
    };

    if status != MAGMA_STATUS_OK {
        response.result_return = status as u64;
    } else {
        let vmo = unsafe { zx::Vmo::from(zx::Handle::from_raw(buffer_handle_out)) };
        let file = Anon::new_file(
            current_task,
            Box::new(VmoFileObject::new(Arc::new(vmo))),
            OpenFlags::RDWR,
        );
        let fd = current_task.add_file(file, FdFlags::empty())?;
        response.handle_out = fd.raw() as u64;
        response.result_return = MAGMA_STATUS_OK as u64;
    }

    response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_GET_HANDLE as u32;

    Ok(())
}

/// Runs a magma query.
///
/// This function will create a new file in `current_task.files` if the magma query returns a VMO
/// handle. The file takes ownership of the VMO handle, and the file descriptor of the file is
/// returned to the client via `response`.
///
/// SAFETY: Makes an FFI call to populate the fields of `response`.
pub fn query(
    current_task: &CurrentTask,
    control: virtio_magma_device_query_ctrl_t,
    response: &mut virtio_magma_device_query_resp_t,
) -> Result<(), Errno> {
    let mut result_buffer_out = 0;
    let mut result_out = 0;
    response.result_return = unsafe {
        magma_device_query(control.device, control.id, &mut result_buffer_out, &mut result_out)
            as u64
    };

    if result_buffer_out != zx::sys::ZX_HANDLE_INVALID {
        let vmo = unsafe { zx::Vmo::from(zx::Handle::from_raw(result_buffer_out)) };
        let vmo_size = vmo.get_size().unwrap();
        let file = Anon::new_file_extended(
            current_task.kernel(),
            Box::new(VmoFileObject::new(Arc::new(vmo))),
            OpenFlags::RDWR,
            |id| {
                let mut info =
                    FsNodeInfo::new(id, FileMode::from_bits(0o600), current_task.as_fscred());
                // Enable seek for file size discovery.
                info.size = vmo_size as usize;
                info
            },
        );
        let fd = current_task.add_file(file, FdFlags::empty())?;
        response.result_buffer_out = fd.raw() as u64;
    } else {
        response.result_buffer_out = u64::MAX;
    }

    response.result_out = result_out;

    Ok(())
}

/// Reads a notification from the connection channel and writes it to `control.buffer`.
///
/// Upon completion, `response.more_data_out` will be true if there is more data waiting to be read.
/// `response.buffer_size_out` contains the size of the returned buffer.
///
/// SAFETY: Makes an FFI call to magma with a buffer that is populated with data. The passed in
/// buffer pointer always points to a valid vector, even if the provided buffer length is 0.
pub fn read_notification_channel(
    current_task: &CurrentTask,
    control: virtio_magma_connection_read_notification_channel_ctrl_t,
    response: &mut virtio_magma_connection_read_notification_channel_resp_t,
    connection: &Arc<MagmaConnection>,
) -> Result<(), Errno> {
    // Buffer has a min length of 1 to make sure the call to
    // `magma_read_notification_channel2` uses a valid reference.
    let mut buffer = vec![0; std::cmp::max(control.buffer_size as usize, 1)];
    let mut buffer_size_out = 0;
    let mut more_data_out: u8 = 0;

    response.result_return = unsafe {
        magma_connection_read_notification_channel(
            connection.handle,
            &mut buffer[0] as *mut u8 as *mut std::ffi::c_void,
            control.buffer_size,
            &mut buffer_size_out,
            &mut more_data_out as *mut u8,
        ) as u64
    };

    response.more_data_out = more_data_out as u64;
    response.buffer_size_out = buffer_size_out;
    response.hdr.type_ =
        virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_READ_NOTIFICATION_CHANNEL as u32;

    current_task.mm.write_memory(UserAddress::from(control.buffer), &buffer)?;

    Ok(())
}

/// Releases the provided `control.connection`.
///
/// # Parameters
///   - `control`: The control message that contains the connection to remove.
///   - `response`: The response message that will be updated to write back to user space.
///   - `connections`: The starnix-magma connection map, which is used to determine whether or not
///                    to call into magma to release the connection.
///
/// SAFETY: Makes an FFI call to populate the fields of `response`.
pub fn release_connection(
    control: virtio_magma_connection_release_ctrl_t,
    response: &mut virtio_magma_connection_release_resp_t,
    connections: &mut ConnectionMap,
) {
    let connection = control.connection as magma_connection_t;
    if connections.contains_key(&connection) {
        connections.remove(&connection);
    }
    response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_RELEASE as u32;
}
