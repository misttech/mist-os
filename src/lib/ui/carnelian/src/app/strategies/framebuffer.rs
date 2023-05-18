// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    app::{
        strategies::base::AppStrategy, BoxedGammaValues, Config, InternalSender, MessageInternal,
    },
    drawing::DisplayRotation,
    geometry::IntSize,
    input::{self, listen_for_user_input, report::InputReportHandler, DeviceId},
    view::{
        strategies::{
            base::{DisplayDirectParams, ViewStrategyParams, ViewStrategyPtr},
            display_direct::DisplayDirectViewStrategy,
        },
        ViewKey,
    },
};
use anyhow::{bail, ensure, Context, Error};
use async_trait::async_trait;
use euclid::size2;
use fidl::endpoints::{self};
use fidl_fuchsia_hardware_display::{
    CoordinatorEvent, CoordinatorMarker, CoordinatorProxy, ProviderMarker, VirtconMode,
};
use fidl_fuchsia_input_report as hid_input_report;
use fidl_fuchsia_ui_scenic::ScenicProxy;
use fuchsia_async::{self as fasync};
use fuchsia_fs::directory as vfs_watcher;
use fuchsia_fs::OpenFlags;
use fuchsia_zircon::{self as zx, Status};
use futures::{channel::mpsc::UnboundedSender, StreamExt, TryFutureExt, TryStreamExt};
use keymaps::Keymap;
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

async fn watch_directory_async(
    dir: PathBuf,
    app_sender: UnboundedSender<MessageInternal>,
) -> Result<(), Error> {
    let dir_proxy = fuchsia_fs::directory::open_in_namespace(
        dir.to_str().expect("to_str"),
        OpenFlags::empty(),
    )?;
    let mut watcher = vfs_watcher::Watcher::new(&dir_proxy).await?;
    fasync::Task::local(async move {
        while let Some(msg) = (watcher.try_next()).await.expect("msg") {
            match msg.event {
                vfs_watcher::WatchEvent::ADD_FILE | vfs_watcher::WatchEvent::EXISTING => {
                    if msg.filename == Path::new(".") {
                        continue;
                    }
                    let device_path = dir.join(msg.filename);
                    app_sender
                        .unbounded_send(MessageInternal::NewDisplayCoordinator(device_path))
                        .expect("unbounded_send");
                }
                _ => (),
            }
        }
    })
    .detach();
    Ok(())
}

pub(crate) struct AutoRepeatContext {
    app_sender: UnboundedSender<MessageInternal>,
    #[allow(unused)]
    keyboard_autorepeat_task: Option<fasync::Task<()>>,
    repeat_interval: std::time::Duration,
}

pub(crate) trait AutoRepeatTimer {
    fn schedule_autorepeat_timer(&mut self, device_id: &DeviceId);
    fn continue_autorepeat_timer(&mut self, device_id: &DeviceId);
    fn cancel_autorepeat_timer(&mut self) {}
}

impl AutoRepeatContext {
    pub(crate) fn new(app_sender: &UnboundedSender<MessageInternal>) -> Self {
        Self {
            app_sender: app_sender.clone(),
            keyboard_autorepeat_task: None,
            repeat_interval: Config::get().keyboard_autorepeat_slow_interval,
        }
    }

    fn schedule(&mut self, device_id: &DeviceId) {
        let timer = fasync::Timer::new(fuchsia_async::Time::after(self.repeat_interval.into()));
        let app_sender = self.app_sender.clone();
        let device_id = device_id.clone();
        let task = fasync::Task::local(async move {
            timer.await;
            app_sender
                .unbounded_send(MessageInternal::KeyboardAutoRepeat(device_id))
                .expect("unbounded_send");
        });
        self.keyboard_autorepeat_task = Some(task);
    }
}

impl AutoRepeatTimer for AutoRepeatContext {
    fn schedule_autorepeat_timer(&mut self, device_id: &DeviceId) {
        self.repeat_interval = Config::get().keyboard_autorepeat_slow_interval;
        self.schedule(device_id);
    }

    fn continue_autorepeat_timer(&mut self, device_id: &DeviceId) {
        self.repeat_interval =
            (self.repeat_interval * 3 / 4).max(Config::get().keyboard_autorepeat_fast_interval);
        self.schedule(device_id);
    }

    fn cancel_autorepeat_timer(&mut self) {
        let task = self.keyboard_autorepeat_task.take();
        if let Some(task) = task {
            fasync::Task::local(async move {
                task.cancel().await;
            })
            .detach();
        }
    }
}

const DISPLAY_COORDINATOR_PATH: &'static str = "/dev/class/display-coordinator";

pub type CoordinatorProxyPtr = Rc<CoordinatorProxy>;

pub fn first_display_device_path() -> Option<PathBuf> {
    let mut entries = fs::read_dir(DISPLAY_COORDINATOR_PATH).ok()?;
    entries.next()?.ok().map(|entry| entry.path())
}

pub struct DisplayCoordinator {
    pub coordinator: CoordinatorProxyPtr,
}

impl DisplayCoordinator {
    pub(crate) async fn open(
        path: &str,
        virtcon_mode: &Option<VirtconMode>,
        app_sender: &UnboundedSender<MessageInternal>,
    ) -> Result<Self, Error> {
        let provider =
            fuchsia_component::client::connect_to_protocol_at_path::<ProviderMarker>(path)
                .context("while opening device file")?;
        let (coordinator, server) = endpoints::create_proxy::<CoordinatorMarker>()?;
        let status = if virtcon_mode.is_some() {
            provider.open_coordinator_for_virtcon(server).await
        } else {
            provider.open_coordinator_for_primary(server).await
        }?;
        ensure!(
            status == zx::sys::ZX_OK,
            "Failed to open display coordinator {}",
            Status::from_raw(status)
        );

        if let Some(virtcon_mode) = virtcon_mode {
            coordinator.set_virtcon_mode(*virtcon_mode)?;
        }

        let mut event_stream = coordinator.take_event_stream();
        let app_sender = app_sender.clone();
        let f = async move {
            loop {
                if let Some(event) = event_stream.next().await {
                    if let Ok(event) = event {
                        app_sender
                            .unbounded_send(MessageInternal::DisplayCoordinatorEvent(event))
                            .expect("unbounded_send");
                    }
                }
            }
        };
        fasync::Task::local(f).detach();
        coordinator.enable_vsync(true).context("enable_vsync failed")?;

        Ok(Self { coordinator: Rc::new(coordinator) })
    }

    pub(crate) async fn watch_displays(app_sender: UnboundedSender<MessageInternal>) {
        watch_directory_async(PathBuf::from(DISPLAY_COORDINATOR_PATH), app_sender)
            .await
            .expect("watch_directory_async");
    }
}

pub type DisplayId = u64;

#[derive(Debug)]
pub struct DisplayInfo {
    preferred_size: IntSize,
    info: fidl_fuchsia_hardware_display::Info,
}

pub(crate) struct DisplayDirectAppStrategy<'a> {
    pub display_coordinator: Option<DisplayCoordinator>,
    pub display_rotation: DisplayRotation,
    pub keymap: &'a Keymap<'a>,
    pub input_report_handlers: HashMap<DeviceId, InputReportHandler<'a>>,
    pub context: AutoRepeatContext,
    pub app_sender: UnboundedSender<MessageInternal>,
    pub owned: bool,
    pub primary_display: Option<DisplayInfo>,
    views: BTreeMap<DisplayId, Vec<ViewKey>>,
}

impl<'a> DisplayDirectAppStrategy<'a> {
    pub fn new(
        display_coordinator: Option<DisplayCoordinator>,
        keymap: &'a Keymap<'a>,
        app_sender: UnboundedSender<MessageInternal>,
        app_config: &Config,
    ) -> DisplayDirectAppStrategy<'a> {
        DisplayDirectAppStrategy {
            display_coordinator: display_coordinator,
            display_rotation: app_config.display_rotation,
            keymap,
            input_report_handlers: HashMap::new(),
            context: AutoRepeatContext::new(&app_sender),
            app_sender,
            owned: false,
            primary_display: None,
            views: Default::default(),
        }
    }

    async fn handle_displays_changed(
        &mut self,
        added: Vec<fidl_fuchsia_hardware_display::Info>,
        removed: Vec<u64>,
    ) -> Result<(), Error> {
        let display_coordinator = self.display_coordinator.as_ref().expect("display_coordinator");
        for display_id in removed {
            self.views.remove(&display_id);
            self.app_sender
                .unbounded_send(MessageInternal::CloseViewsOnDisplay(display_id))
                .expect("unbounded");
        }

        for info in added {
            // We use the preferred mode of the first display as the preferred size.
            // This makes it more likely that input events will translate across
            // different displays.
            let preferred_size = self
                .primary_display
                .get_or_insert_with(|| {
                    let mode = info.modes[0];
                    DisplayInfo {
                        preferred_size: size2(mode.horizontal_resolution, mode.vertical_resolution)
                            .to_i32(),
                        info: info.clone(),
                    }
                })
                .preferred_size;
            self.app_sender
                .unbounded_send(MessageInternal::CreateView(ViewStrategyParams::DisplayDirect(
                    DisplayDirectParams {
                        view_key: None,
                        coordinator: display_coordinator.coordinator.clone(),
                        info,
                        preferred_size,
                    },
                )))
                .expect("send");
        }

        Ok(())
    }
}

#[async_trait(?Send)]
impl<'a> AppStrategy for DisplayDirectAppStrategy<'a> {
    async fn create_view_strategy(
        &mut self,
        key: ViewKey,
        app_sender: UnboundedSender<MessageInternal>,
        strategy_params: ViewStrategyParams,
    ) -> Result<ViewStrategyPtr, Error> {
        let strategy_params = match strategy_params {
            ViewStrategyParams::DisplayDirect(params) => params,
            _ => bail!(
                "Incorrect ViewStrategyParams passed to create_view_strategy for frame buffer"
            ),
        };
        let views_on_display = self.views.entry(strategy_params.info.id).or_default();
        views_on_display.push(key);
        Ok(DisplayDirectViewStrategy::new(
            key,
            strategy_params.coordinator,
            app_sender.clone(),
            strategy_params.info,
            strategy_params.preferred_size,
        )
        .await?)
    }

    fn create_view_strategy_params_for_additional_view(
        &mut self,
        view_key: ViewKey,
    ) -> ViewStrategyParams {
        let primary_display = self.primary_display.as_ref().expect("primary_display");
        ViewStrategyParams::DisplayDirect(DisplayDirectParams {
            view_key: Some(view_key),
            coordinator: self
                .display_coordinator
                .as_ref()
                .expect("display_coordinator")
                .coordinator
                .clone(),
            info: primary_display.info.clone(),
            preferred_size: primary_display.preferred_size,
        })
    }

    fn supports_scenic(&self) -> bool {
        return false;
    }

    fn get_scenic_proxy(&self) -> Option<&ScenicProxy> {
        return None;
    }

    async fn post_setup(&mut self, internal_sender: &InternalSender) -> Result<(), Error> {
        let input_report_sender = internal_sender.clone();
        fasync::Task::local(
            listen_for_user_input(input_report_sender)
                .unwrap_or_else(|e: anyhow::Error| eprintln!("error: listening for input {:?}", e)),
        )
        .detach();
        Ok(())
    }

    fn handle_input_report(
        &mut self,
        device_id: &input::DeviceId,
        input_report: &hid_input_report::InputReport,
    ) -> Vec<input::Event> {
        let handler = self.input_report_handlers.get_mut(device_id).expect("input_report_handler");
        handler.handle_input_report(device_id, input_report, &mut self.context)
    }

    fn handle_register_input_device(
        &mut self,
        device_id: &input::DeviceId,
        device_descriptor: &hid_input_report::DeviceDescriptor,
    ) {
        let frame_buffer_size =
            self.primary_display.as_ref().expect("primary_display").preferred_size;
        self.input_report_handlers.insert(
            device_id.clone(),
            InputReportHandler::new(
                device_id.clone(),
                frame_buffer_size,
                self.display_rotation,
                device_descriptor,
                self.keymap,
            ),
        );
    }

    fn handle_keyboard_autorepeat(&mut self, device_id: &input::DeviceId) -> Vec<input::Event> {
        let handler = self.input_report_handlers.get_mut(device_id).expect("input_report_handler");
        handler.handle_keyboard_autorepeat(device_id, &mut self.context)
    }

    async fn handle_new_display_coordinator(&mut self, display_path: PathBuf) {
        if self.display_coordinator.is_none() {
            let display_coordinator = DisplayCoordinator::open(
                display_path.to_str().unwrap(),
                &Config::get().virtcon_mode,
                &self.app_sender,
            )
            .await
            .expect("DisplayCoordinator::open");
            self.display_coordinator = Some(display_coordinator);
        }
    }

    async fn handle_display_coordinator_event(&mut self, event: CoordinatorEvent) {
        match event {
            CoordinatorEvent::OnDisplaysChanged { added, removed } => {
                self.handle_displays_changed(added, removed)
                    .await
                    .expect("handle_displays_changed");
            }
            CoordinatorEvent::OnClientOwnershipChange { has_ownership } => {
                self.owned = has_ownership;
                self.app_sender
                    .unbounded_send(MessageInternal::OwnershipChanged(has_ownership))
                    .expect("unbounded_send");
            }
            CoordinatorEvent::OnVsync { .. } => {
                panic!("App strategy should not see vsync events");
            }
        }
    }

    fn set_virtcon_mode(&mut self, virtcon_mode: VirtconMode) {
        self.display_coordinator
            .as_ref()
            .expect("display_coordinator")
            .coordinator
            .set_virtcon_mode(virtcon_mode)
            .expect("set_virtcon_mode");
    }

    fn import_and_set_gamma_table(
        &mut self,
        display_id: u64,
        gamma_table_id: u64,
        r: BoxedGammaValues,
        g: BoxedGammaValues,
        b: BoxedGammaValues,
    ) {
        let display_coordinator = self.display_coordinator.as_ref().expect("display_coordinator");
        display_coordinator
            .coordinator
            .import_gamma_table(gamma_table_id, &r, &g, &b)
            .expect("import_gamma_table");
        display_coordinator
            .coordinator
            .set_display_gamma_table(display_id, gamma_table_id)
            .expect("set_display_gamma_table");
    }

    fn get_focused_view_key(&self) -> Option<ViewKey> {
        self.views.keys().next().and_then(|first_display| {
            self.views.get(first_display).expect("first_display").last().cloned()
        })
    }

    fn get_visible_view_key_for_display(&self, display_id: DisplayId) -> Option<ViewKey> {
        self.views
            .get(&display_id)
            .and_then(|views_on_first_display| views_on_first_display.last().cloned())
    }

    fn handle_view_closed(&mut self, view_key: ViewKey) {
        for views_on_display in self.views.values_mut() {
            views_on_display.retain(|a_view_key| view_key != *a_view_key);
        }
    }
}
