// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::framebuffer_server::{
    init_viewport_scene, send_view_to_graphical_presenter, spawn_view_provider,
    start_flatland_presentation_loop, FramebufferServer,
};
use crate::{
    device::{features::AspectRatio, kobject::KObjectDeviceAttribute, DeviceMode, DeviceOps},
    mm::{MemoryAccessorExt, ProtectionFlags},
    task::{CurrentTask, Kernel},
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_seekable, FileObject, FileOps, FsNode, VmoFileOperation,
    },
};
use fidl_fuchsia_io as fio;
use fidl_fuchsia_math as fmath;
use fidl_fuchsia_ui_composition as fuicomposition;
use fidl_fuchsia_ui_display_singleton as fuidisplay;
use fidl_fuchsia_ui_views as fuiviews;
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_fs::directory as ffs_dir;
use fuchsia_zircon as zx;
use starnix_logging::{impossible_error, log_info, log_warn};
use starnix_sync::RwLock;
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    device_type::DeviceType,
    errno, error,
    errors::Errno,
    fb_bitfield, fb_fix_screeninfo, fb_var_screeninfo,
    open_flags::OpenFlags,
    user_address::{UserAddress, UserRef},
    FBIOGET_FSCREENINFO, FBIOGET_VSCREENINFO, FBIOPUT_VSCREENINFO, FB_TYPE_PACKED_PIXELS,
    FB_VISUAL_TRUECOLOR,
};
use std::sync::Arc;
use zerocopy::AsBytes;

pub struct Framebuffer {
    vmo: Arc<zx::Vmo>,
    vmo_len: u32,
    pub info: RwLock<fb_var_screeninfo>,
    server: Option<Arc<FramebufferServer>>,
}

impl Framebuffer {
    /// Creates a new `Framebuffer` fit to the screen, while maintaining the provided aspect ratio.
    ///
    /// If the `aspect_ratio` is `None`, the framebuffer will be scaled to the display.
    pub fn new(aspect_ratio: Option<&AspectRatio>) -> Result<Arc<Self>, Errno> {
        let mut info = fb_var_screeninfo::default();

        let display_size =
            Self::get_display_size().unwrap_or(fmath::SizeU { width: 700, height: 1200 });

        // If the container has a specific aspect ratio set, use that to fit the framebuffer
        // inside of the display.
        let (feature_width, feature_height) = aspect_ratio
            .map(|ar| (ar.width, ar.height))
            .unwrap_or((display_size.width, display_size.height));

        // Scale to framebuffer to fit the display, while maintaining the expected aspect ratio.
        let ratio =
            std::cmp::min(display_size.width / feature_width, display_size.height / feature_height);
        let (width, height) = (feature_width * ratio, feature_height * ratio);

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

            Ok(Arc::new(Self { vmo, vmo_len, server: Some(server), info: RwLock::new(info) }))
        } else {
            let vmo_len = info.xres * info.yres * (info.bits_per_pixel / 8);
            let vmo = Arc::new(zx::Vmo::create(vmo_len as u64).map_err(|s| match s {
                zx::Status::NO_MEMORY => errno!(ENOMEM),
                _ => impossible_error(s),
            })?);
            Ok(Arc::new(Self { vmo, vmo_len, server: None, info: RwLock::new(info) }))
        }
    }

    /// Starts presenting a view based on this framebuffer.
    /// If GraphicalPresenter is detected as an incoming service among
    /// `maybe_svc`, connect to that protocol and PresentView. Otherwise, serve
    /// ViewProvider. This is a transitionary measure while ViewProvider is
    /// being deprecated.
    ///
    /// # Parameters
    /// * `view_bound_protocols`: handles to input clients which will be
    ///    associated with the view
    /// * `view_identify`: the identity used to create view with flatland
    /// * `outgoing_dir`: the path under which the `ViewProvider` protocol will
    ///    be served
    /// * `maybe_svc`: the incoming service directory under which the
    ///    `GraphicalPresenter` protocol might be retrieved.
    pub fn start_server(
        &self,
        kernel: &Arc<Kernel>,
        view_bound_protocols: fuicomposition::ViewBoundProtocols,
        view_identity: fuiviews::ViewIdentityOnCreation,
        outgoing_dir: fidl::endpoints::ServerEnd<fio::DirectoryMarker>,
        maybe_svc: Option<fio::DirectorySynchronousProxy>,
    ) -> Result<(), anyhow::Error> {
        if let Some(server) = &self.server {
            // Start presentation loop to prepare for display updates.
            start_flatland_presentation_loop(kernel, server.clone());

            // Attempt to find and connect to GraphicalPresenter.
            //
            // TODO: b/307788344 - DirectorySynchronousProxy is not ideal,
            // since it blocks this thread until the FIDL call returns. Use
            // DirectoryProxy instead of DirectorySynchronousProxy when we
            // remove spawn_view_provider.
            if let Some(svc_dir_proxy) = maybe_svc {
                let (status, buf) = svc_dir_proxy
                    .read_dirents(fio::MAX_BUF, zx::Time::INFINITE)
                    .expect("Calling read dirents");
                zx::Status::ok(status).expect("Failed reading directory entries");
                for entry in ffs_dir::parse_dir_entries(&buf)
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .expect("Failed parsing directory entries")
                {
                    if entry.name == "fuchsia.element.GraphicalPresenter" {
                        log_info!("Presenting view using GraphicalPresenter");
                        send_view_to_graphical_presenter(
                            kernel,
                            server.clone(),
                            view_bound_protocols,
                            view_identity,
                            svc_dir_proxy,
                        );
                        return Ok(());
                    }
                }
            }

            // Fallback to serving ViewProvider.
            log_info!("Serving ViewProvider");
            spawn_view_provider(
                kernel,
                server.clone(),
                view_bound_protocols,
                view_identity,
                outgoing_dir,
            );
        }

        Ok(())
    }

    /// Starts presenting a child view instead of the framebuffer.
    ///
    /// # Parameters
    /// * `viewport_token`: handles to the child view
    pub fn present_view(&self, viewport_token: fuiviews::ViewportCreationToken) {
        if let Some(server) = &self.server {
            init_viewport_scene(server.clone(), viewport_token);
        }
    }

    fn get_display_size() -> Result<fmath::SizeU, Errno> {
        let singleton_display_info =
            connect_to_protocol_sync::<fuidisplay::InfoMarker>().map_err(|_| errno!(ENOENT))?;
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
                current_task.write_object(UserRef::new(user_addr), &finfo)?;
                Ok(SUCCESS)
            }

            FBIOGET_VSCREENINFO => {
                let info = self.info.read();
                current_task.write_object(UserRef::new(user_addr), &*info)?;
                Ok(SUCCESS)
            }

            FBIOPUT_VSCREENINFO => {
                let new_info: fb_var_screeninfo =
                    current_task.read_object(UserRef::new(user_addr))?;
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
        VmoFileOperation::read(&self.vmo, file, offset, data)
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        VmoFileOperation::write(&self.vmo, file, current_task, offset, data)
    }

    fn get_vmo(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        _length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        VmoFileOperation::get_vmo(&self.vmo, file, current_task, prot)
    }
}

pub fn fb_device_init(system_task: &CurrentTask) {
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;

    let graphics_class = registry.add_class(b"graphics", registry.virtual_bus());
    let fb_attr = KObjectDeviceAttribute::new(
        None,
        graphics_class,
        b"fb0",
        b"fb0",
        DeviceType::FB0,
        DeviceMode::Char,
    );
    registry.add_and_register_device(system_task, fb_attr, kernel.framebuffer.clone());
}
