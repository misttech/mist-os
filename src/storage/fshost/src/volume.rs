// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::device::Device,
    anyhow::{anyhow, Context, Error},
    fidl_fuchsia_hardware_block_partition::Guid,
    fidl_fuchsia_hardware_block_volume::{VolumeManagerMarker, VolumeProxy},
    fuchsia_component::client::connect_to_protocol_at_path,
    fuchsia_zircon as zx,
    std::cmp,
};

// Number of bits required for the VSlice address space.
const SLICE_ENTRY_VSLICE_BITS: u64 = 32;
// Maximum number of VSlices that can be addressed.
const MAX_VSLICES: u64 = 1 << (SLICE_ENTRY_VSLICE_BITS - 1);
const DEFAULT_VOLUME_PERCENTAGE: u64 = 10;
const DEFAULT_VOLUME_SIZE: u64 = 24 * 1024 * 1024;

pub async fn resize_volume(volume_proxy: &VolumeProxy, target_bytes: u64) -> Result<u64, Error> {
    // Free existing slices (except the first).
    // Note while physical slices in FVM are 1-indexed, virtual slices (used here) are 0-indexed.
    // The reason we start at slice 1 here is because FVM requires that the first slice always be
    // allocated -- we can't shrink it away.
    let mut slice = 1;
    while slice < (MAX_VSLICES - 1) {
        // Note also that query_slices responds with an extent that is either allocated or free,
        // but not a mix of both. The last unallocated extent always has a slice count that runs
        // the total to MAX_VSLICES - 1.
        let (status, vslices, response_count) =
            volume_proxy.query_slices(&[slice]).await.context("Transport error on query_slices")?;
        zx::Status::ok(status).context("query_slices failed")?;
        if response_count == 0 {
            break;
        }
        for i in 0..response_count {
            if vslices[i as usize].allocated {
                let status = volume_proxy
                    .shrink(slice, vslices[i as usize].count)
                    .await
                    .context("Transport error on shrink")?;
                zx::Status::ok(status)?;
            }
            slice += vslices[i as usize].count;
        }
    }
    let (status, volume_manager_info, _volume_info) =
        volume_proxy.get_volume_info().await.context("Transport error on get_volume_info")?;
    zx::Status::ok(status).context("get_volume_info failed")?;
    let manager = volume_manager_info.ok_or(anyhow!("Expected volume manager info"))?;
    let slice_size = manager.slice_size;

    // Note we add one here to include the 0th slice that we cannot shrink away.
    let slices_available = manager.slice_count - manager.assigned_slice_count + 1;
    let mut slice_count = if target_bytes == 0 {
        // If a size is not specified, limit the size of the data partition so as not to use up all
        // FVM's space (thus limiting blobfs growth).  10% or 24MiB (whichever is larger) should be
        // enough.
        let default_slices = cmp::max(
            manager.slice_count * DEFAULT_VOLUME_PERCENTAGE / 100,
            DEFAULT_VOLUME_SIZE / slice_size,
        );
        tracing::info!("Using default size of {:?}", default_slices * slice_size);
        cmp::min(slices_available, default_slices)
    } else {
        (target_bytes + slice_size - 1) / slice_size
    };
    if slices_available < slice_count {
        tracing::info!(
            "Only {:?} slices available. Some functionality
                may be missing",
            slices_available
        );
        slice_count = slices_available;
    }
    if slice_count > 1 {
        let status = volume_proxy
            .extend(1, slice_count - 1)
            .await
            .context("Transport error on extend call")?;
        zx::Status::ok(status)
            .context(format!("Failed to extend partition (slice count: {:?})", slice_count))?;
    }
    return Ok(slice_count * slice_size);
}

pub async fn set_partition_max_bytes(
    device: &mut dyn Device,
    max_byte_size: u64,
) -> Result<(), Error> {
    if max_byte_size == 0 {
        return Ok(());
    }

    let index =
        device.topological_path().rfind("/fvm").ok_or(anyhow!("fvm is not in the device path"))?;
    // The 4 is from the 4 characters in "/fvm"
    let fvm_path = &device.topological_path()[..index + 4];

    let fvm_proxy = connect_to_protocol_at_path::<VolumeManagerMarker>(&fvm_path)
        .context("Failed to connect to fvm volume manager")?;
    let (status, info) = fvm_proxy.get_info().await.context("Transport error in get_info call")?;
    zx::Status::ok(status).context("get_info call failed")?;
    let info = info.ok_or(anyhow!("Expected info"))?;
    let slice_size = info.slice_size;
    let max_slice_count = (max_byte_size + slice_size - 1) / slice_size;
    let instance_guid =
        Guid { value: *device.partition_instance().await.context("Expected partition instance")? };
    let status = fvm_proxy
        .set_partition_limit(&instance_guid, max_slice_count)
        .await
        .context("Transport error on set_partition_limit")?;
    zx::Status::ok(status).context("set_partition_limit failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        crate::volume::{resize_volume, MAX_VSLICES},
        anyhow::Error,
        fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_hardware_block_volume::{
            VolumeManagerInfo, VolumeMarker, VolumeRequest, VsliceRange,
        },
        fuchsia_zircon as zx,
        futures::{pin_mut, select, FutureExt, StreamExt},
    };

    const SLICE_SIZE: u64 = 16384;
    const SLICE_COUNT: u64 = 5000;
    const MAXIMUM_SLICE_COUNT: u64 = 5500;
    const RANGE_ALLOCATED: u64 = 1234;

    async fn check_resize_volume(
        target_bytes: u64,
        assigned_slice_count: u64,
        expected_extend_slice_count: u64,
    ) -> Result<u64, Error> {
        let (proxy, mut stream) = create_proxy_and_stream::<VolumeMarker>().unwrap();
        let mock_device = async {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(VolumeRequest::QuerySlices { responder, start_slices }) => {
                        let mut slices = [VsliceRange { allocated: false, count: 0 }; 16];
                        slices[0] = VsliceRange { allocated: true, count: RANGE_ALLOCATED };
                        let count = if start_slices[0] == 1 { 1 } else { 0 };
                        responder.send(zx::sys::ZX_OK, &slices, count).unwrap();
                    }
                    Ok(VolumeRequest::Shrink { responder, start_slice, slice_count }) => {
                        assert_eq!(start_slice, 1);
                        assert_eq!(slice_count, RANGE_ALLOCATED);
                        responder.send(zx::sys::ZX_OK).unwrap();
                    }
                    Ok(VolumeRequest::GetVolumeInfo { responder }) => {
                        responder
                            .send(
                                zx::sys::ZX_OK,
                                Some(&VolumeManagerInfo {
                                    slice_size: SLICE_SIZE,
                                    slice_count: SLICE_COUNT,
                                    assigned_slice_count: assigned_slice_count,
                                    maximum_slice_count: MAXIMUM_SLICE_COUNT,
                                    max_virtual_slice: MAX_VSLICES,
                                }),
                                None,
                            )
                            .unwrap();
                    }
                    Ok(VolumeRequest::Extend { responder, start_slice, slice_count }) => {
                        assert_eq!(start_slice, 1);
                        assert_eq!(slice_count, expected_extend_slice_count);
                        responder.send(zx::sys::ZX_OK).unwrap();
                    }
                    _ => {
                        println!("Unexpected request: {:?}", request);
                        unreachable!()
                    }
                }
            }
        }
        .fuse();

        pin_mut!(mock_device);

        select! {
            _ = mock_device => unreachable!(),
            matches = resize_volume(&proxy, target_bytes).fuse() => matches,
        }
    }

    #[fuchsia::test]
    async fn test_target_bytes_zero_slice_count_equals_default_slices() {
        let target_bytes = 0;
        let assigned_slice_count = 3000;
        let expected_extend_slice_count = 1535;
        assert_eq!(
            check_resize_volume(target_bytes, assigned_slice_count, expected_extend_slice_count)
                .await
                .unwrap(),
            // Add one because extend ignores free allocated slice per volume
            (expected_extend_slice_count + 1) * SLICE_SIZE
        );
    }

    #[fuchsia::test]
    async fn test_target_bytes_zero_slice_count_equals_slices_available() {
        let target_bytes = 0;
        let assigned_slice_count = 4000;
        let expected_extend_slice_count = 1000;
        assert_eq!(
            check_resize_volume(target_bytes, assigned_slice_count, expected_extend_slice_count)
                .await
                .unwrap(),
            // Add one because extend ignores free allocated slice per volume
            (expected_extend_slice_count + 1) * SLICE_SIZE
        );
    }

    #[fuchsia::test]
    async fn test_slice_count_less_than_slice_available() {
        let target_bytes = SLICE_SIZE * 2500;
        let assigned_slice_count = 3000;
        let expected_extend_slice_count = 2000;
        assert_eq!(
            check_resize_volume(target_bytes, assigned_slice_count, expected_extend_slice_count)
                .await
                .unwrap(),
            // Add one because extend ignores free allocated slice per volume
            (expected_extend_slice_count + 1) * SLICE_SIZE
        );
    }
}
