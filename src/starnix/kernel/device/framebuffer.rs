// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::framebuffer_server::{spawn_view_provider, FramebufferServer};
use crate::{
    device::{input::InputFile, DeviceOps},
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        *,
    },
    lock::RwLock,
    logging::*,
    mm::{MemoryAccessorExt, ProtectionFlags},
    syscalls::*,
    task::CurrentTask,
    types::*,
};

use fidl_fuchsia_math as fmath;
use fidl_fuchsia_ui_composition as fuicomposition;
use fidl_fuchsia_ui_display_singleton as fuidisplay;
use fuchsia_component::client::connect_channel_to_protocol;
use fuchsia_zircon as zx;
use std::sync::Arc;
use zerocopy::AsBytes;

pub struct Framebuffer {
    vmo: Arc<zx::Vmo>,
    vmo_len: u32,
    info: RwLock<fb_var_screeninfo>,
    server: Option<Arc<FramebufferServer>>,
}

impl Framebuffer {
    /// Creates a new `Framebuffer` according to the spec provided in `feature_string`.
    ///
    /// This also creates an `InputFile` that is set up to detect input within the bounds
    /// of the framebuffer.
    ///
    /// For example, `aspect_ratio:1:1` creates a 1:1 aspect ratio framebuffer, scaled to
    /// fit the display.
    ///
    /// If the `feature_string` is empty, or `None`, the framebuffer will be scaled to the
    /// display.
    pub fn new_with_input(
        feature_string: Option<&String>,
    ) -> Result<(Arc<Self>, Arc<InputFile>), Errno> {
        let mut info = fb_var_screeninfo::default();

        let display_size =
            Self::get_display_size().unwrap_or(fmath::SizeU { width: 700, height: 1200 });

        // If the container has a specific aspect ratio set, use that to fit the framebuffer
        // inside of the display.
        let (feature_width, feature_height) = feature_string
            .map(|s| {
                let components: Vec<_> = s.split(':').collect();
                assert_eq!(components.len(), 3, "Malformed aspect ratio");
                (
                    components[1].parse().expect("Malformed aspect ratio"),
                    components[2].parse().expect("Malformed aspect ratio"),
                )
            })
            .unwrap_or((display_size.width, display_size.height));

        // Scale to framebuffer to fit the display, while maintaining the expected aspect ratio.
        let ratio =
            std::cmp::min(display_size.width / feature_width, display_size.height / feature_height);
        let (width, height) = (feature_width * ratio, feature_height * ratio);

        let input_file = InputFile::new(width, height);

        info.xres = width;
        info.yres = height;
        info.xres_virtual = info.xres;
        info.yres_virtual = info.yres;
        info.bits_per_pixel = 32;
        info.red = fb_bitfield { offset: 0, length: 8, msb_right: 0 };
        info.green = fb_bitfield { offset: 8, length: 8, msb_right: 0 };
        info.blue = fb_bitfield { offset: 16, length: 8, msb_right: 0 };
        info.transp = fb_bitfield { offset: 24, length: 8, msb_right: 0 };

        if let Ok(server) = FramebufferServer::new(width, height) {
            let server = Arc::new(server);
            let vmo = Arc::new(server.get_vmo()?);
            let vmo_len = vmo.info().map_err(|_| errno!(EINVAL))?.size_bytes as u32;
            // Fill the buffer with white pixels as a placeholder.
            if let Err(err) = vmo.write(&vec![0xff; vmo_len as usize], 0) {
                log_warn!("could not write initial framebuffer: {:?}", err);
            }

            Ok((
                Arc::new(Self { vmo, vmo_len, server: Some(server), info: RwLock::new(info) }),
                input_file,
            ))
        } else {
            let vmo_len = info.xres * info.yres * (info.bits_per_pixel / 8);
            let vmo = Arc::new(zx::Vmo::create(vmo_len as u64).map_err(|s| match s {
                zx::Status::NO_MEMORY => errno!(ENOMEM),
                _ => impossible_error(s),
            })?);
            Ok((Arc::new(Self { vmo, vmo_len, server: None, info: RwLock::new(info) }), input_file))
        }
    }

    /// Starts serving a view based on this framebuffer in `outgoing_dir`.
    ///
    /// # Parameters
    /// * `view_bound_protocols`: handles to input clients which will be associated with the view
    /// * `outgoing_dir`: the path under which the `ViewProvider` protocol will be served
    pub fn start_server(
        &self,
        view_bound_protocols: fuicomposition::ViewBoundProtocols,
        outgoing_dir: fidl::endpoints::ServerEnd<fidl_fuchsia_io::DirectoryMarker>,
    ) {
        if let Some(server) = &self.server {
            spawn_view_provider(server.clone(), view_bound_protocols, outgoing_dir);
        }
    }

    fn get_display_size() -> Result<fmath::SizeU, Errno> {
        let (server_end, client_end) = zx::Channel::create();
        connect_channel_to_protocol::<fuidisplay::InfoMarker>(server_end)
            .map_err(|_| errno!(ENOENT))?;
        let singleton_display_info = fuidisplay::InfoSynchronousProxy::new(client_end);
        let metrics =
            singleton_display_info.get_metrics(zx::Time::INFINITE).map_err(|_| errno!(EINVAL))?;
        let extent_in_px =
            metrics.extent_in_px.ok_or("Failed to get extent_in_px").map_err(|_| errno!(EINVAL))?;
        Ok(extent_in_px)
    }
}

impl DeviceOps for Arc<Framebuffer> {
    fn open(
        &self,
        _current_task: &CurrentTask,
        dev: DeviceType,
        node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        if dev.minor() != 0 {
            return error!(ENODEV);
        }
        node.update_info(|info| {
            info.size = self.vmo_len as usize;
            info.blocks = self.vmo.get_size().map_err(impossible_error)? as usize / info.blksize;
            Ok(())
        })?;
        Ok(Box::new(Arc::clone(self)))
    }
}

impl FileOps for Arc<Framebuffer> {
    fileops_impl_seekable!();

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        match request {
            FBIOGET_FSCREENINFO => {
                let info = self.info.read();
                let finfo = fb_fix_screeninfo {
                    id: zerocopy::FromBytes::read_from(&b"Starnix\0\0\0\0\0\0\0\0\0"[..]).unwrap(),
                    smem_start: 0,
                    smem_len: self.vmo_len,
                    type_: FB_TYPE_PACKED_PIXELS,
                    visual: FB_VISUAL_TRUECOLOR,
                    line_length: info.bits_per_pixel / 8 * info.xres,
                    ..fb_fix_screeninfo::default()
                };
                current_task.mm.write_object(UserRef::new(user_addr), &finfo)?;
                Ok(SUCCESS)
            }

            FBIOGET_VSCREENINFO => {
                let info = self.info.read();
                current_task.mm.write_object(UserRef::new(user_addr), &*info)?;
                Ok(SUCCESS)
            }

            FBIOPUT_VSCREENINFO => {
                let new_info: fb_var_screeninfo =
                    current_task.mm.read_object(UserRef::new(user_addr))?;
                let old_info = self.info.read();
                // We don't yet support actually changing anything
                if new_info.as_bytes() != old_info.as_bytes() {
                    return error!(EINVAL);
                }
                Ok(SUCCESS)
            }

            _ => {
                error!(EINVAL)
            }
        }
    }

    fn read(
        &self,
        file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        VmoFileObject::read(&self.vmo, file, offset, data)
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        VmoFileObject::write(&self.vmo, file, current_task, offset, data, None)
    }

    fn get_vmo(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        _length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        VmoFileObject::get_vmo(&self.vmo, file, current_task, prot)
    }
}
