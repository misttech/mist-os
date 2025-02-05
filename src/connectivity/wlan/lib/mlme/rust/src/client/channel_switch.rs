// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::client::scanner::Scanner;
use crate::client::{Context, TimedEvent};
use crate::device::DeviceOps;
use anyhow::bail;
use futures::Future;
use log::error;
use wlan_common::mac::BeaconHdr;
use wlan_common::timer::EventId;
use wlan_common::{ie, TimeUnit};
use zerocopy::SplitByteSlice;
use {fidl_fuchsia_wlan_common as fidl_common, fuchsia_async as fasync};

pub trait ChannelActions {
    fn switch_channel(
        &mut self,
        new_main_channel: fidl_common::WlanChannel,
    ) -> impl Future<Output = Result<(), zx::Status>>;
    fn schedule_channel_switch_timeout(&mut self, time: zx::MonotonicInstant) -> EventId;
    fn disable_scanning(&mut self) -> impl Future<Output = Result<(), zx::Status>>;
    fn enable_scanning(&mut self);
    fn disable_tx(&mut self) -> Result<(), zx::Status>;
    fn enable_tx(&mut self);
}

pub struct ChannelActionHandle<'a, D> {
    ctx: &'a mut Context<D>,
    scanner: &'a mut Scanner,
}

impl<'a, D: DeviceOps> ChannelActions for ChannelActionHandle<'a, D> {
    async fn switch_channel(
        &mut self,
        new_main_channel: fidl_common::WlanChannel,
    ) -> Result<(), zx::Status> {
        self.ctx.device.set_channel(new_main_channel).await
    }
    fn schedule_channel_switch_timeout(&mut self, time: zx::MonotonicInstant) -> EventId {
        self.ctx.timer.schedule_at(time, TimedEvent::ChannelSwitch)
    }
    async fn disable_scanning(&mut self) -> Result<(), zx::Status> {
        let mut bound_scanner = self.scanner.bind(self.ctx);
        bound_scanner.disable_scanning().await
    }
    fn enable_scanning(&mut self) {
        let mut bound_scanner = self.scanner.bind(self.ctx);
        bound_scanner.enable_scanning()
    }
    fn disable_tx(&mut self) -> Result<(), zx::Status> {
        // TODO(https://fxbug.dev/42060974): Support transmission pause.
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn enable_tx(&mut self) {}
}

#[derive(Default)]
pub struct ChannelState {
    // The current main channel configured in the driver. If None, the driver may
    // be set to any channel.
    main_channel: Option<fidl_common::WlanChannel>,
    pending_channel_switch: Option<(ChannelSwitch, EventId)>,
    beacon_interval: Option<TimeUnit>,
    last_beacon_timestamp: Option<fasync::MonotonicInstant>,
}

pub struct BoundChannelState<'a, T> {
    channel_state: &'a mut ChannelState,
    actions: T,
}

impl ChannelState {
    #[cfg(test)]
    pub fn new_with_main_channel(main_channel: fidl_common::WlanChannel) -> Self {
        Self { main_channel: Some(main_channel), ..Default::default() }
    }

    pub fn get_main_channel(&self) -> Option<fidl_common::WlanChannel> {
        self.main_channel
    }

    pub fn bind<'a, D>(
        &'a mut self,
        ctx: &'a mut Context<D>,
        scanner: &'a mut Scanner,
    ) -> BoundChannelState<'a, ChannelActionHandle<'a, D>> {
        BoundChannelState { channel_state: self, actions: ChannelActionHandle { ctx, scanner } }
    }

    #[cfg(test)]
    pub fn test_bind<'a, T: ChannelActions>(&'a mut self, actions: T) -> BoundChannelState<'a, T> {
        BoundChannelState { channel_state: self, actions }
    }

    fn channel_switch_time_from_count(&self, channel_switch_count: u8) -> fasync::MonotonicInstant {
        let beacon_interval =
            self.beacon_interval.clone().unwrap_or(TimeUnit::DEFAULT_BEACON_INTERVAL);
        let beacon_duration = fasync::MonotonicDuration::from(beacon_interval);
        let duration = beacon_duration * channel_switch_count;
        let now = fasync::MonotonicInstant::now();
        let mut last_beacon =
            self.last_beacon_timestamp.unwrap_or_else(|| fasync::MonotonicInstant::now());
        // Calculate the theoretical latest beacon timestamp before now.
        // Note this may be larger than last_beacon_timestamp if a beacon frame was missed.
        while now - last_beacon > beacon_duration {
            last_beacon += beacon_duration;
        }
        last_beacon + duration
    }
}

impl<'a, T: ChannelActions> BoundChannelState<'a, T> {
    /// Immediately set a new main channel in the device.
    pub async fn set_main_channel(
        &mut self,
        new_main_channel: fidl_common::WlanChannel,
    ) -> Result<(), zx::Status> {
        self.channel_state.pending_channel_switch.take();
        let result = self.actions.switch_channel(new_main_channel).await;
        match result {
            Ok(()) => {
                log::info!("Switched to new main channel {:?}", new_main_channel);
                self.channel_state.main_channel.replace(new_main_channel);
            }
            Err(e) => {
                log::error!("Failed to switch to new main channel {:?}: {}", new_main_channel, e);
            }
        }
        self.actions.enable_scanning();
        self.actions.enable_tx();
        result
    }

    /// Clear the main channel, disable any channel switches, and return to a
    /// normal idle state. The device will remain on whichever channel was
    /// most recently configured.
    pub fn clear_main_channel(&mut self) {
        self.channel_state.main_channel.take();
        self.channel_state.pending_channel_switch.take();
        self.channel_state.last_beacon_timestamp.take();
        self.channel_state.beacon_interval.take();
        self.actions.enable_scanning();
        self.actions.enable_tx();
    }

    pub async fn handle_beacon(
        &mut self,
        header: &BeaconHdr,
        elements: &[u8],
    ) -> Result<(), anyhow::Error> {
        self.channel_state.last_beacon_timestamp.replace(fasync::MonotonicInstant::now());
        self.channel_state.beacon_interval.replace(header.beacon_interval);
        self.handle_channel_switch_elements_if_present(elements, false).await
    }

    pub async fn handle_announcement_frame(
        &mut self,
        elements: &[u8],
    ) -> Result<(), anyhow::Error> {
        self.handle_channel_switch_elements_if_present(elements, true).await
    }

    async fn handle_channel_switch_elements_if_present(
        &mut self,
        elements: &[u8],
        action_frame: bool,
    ) -> Result<(), anyhow::Error> {
        let mut csa_builder = ChannelSwitchBuilder::<&[u8]>::default();
        for (ie_type, range) in ie::IeSummaryIter::new(elements) {
            match ie_type {
                ie::IeType::CHANNEL_SWITCH_ANNOUNCEMENT => {
                    let csa = ie::parse_channel_switch_announcement(&elements[range])?;
                    csa_builder.add_channel_switch_announcement((*csa).clone());
                }
                ie::IeType::SECONDARY_CHANNEL_OFFSET => {
                    let sco = ie::parse_sec_chan_offset(&elements[range])?;
                    csa_builder.add_secondary_channel_offset((*sco).clone());
                }
                ie::IeType::EXTENDED_CHANNEL_SWITCH_ANNOUNCEMENT => {
                    let ecsa = ie::parse_extended_channel_switch_announcement(&elements[range])?;
                    csa_builder.add_extended_channel_switch_announcement((*ecsa).clone());
                }
                ie::IeType::CHANNEL_SWITCH_WRAPPER => {
                    let csw = ie::parse_channel_switch_wrapper(&elements[range])?;
                    csa_builder.add_channel_switch_wrapper(csw);
                }
                ie::IeType::WIDE_BANDWIDTH_CHANNEL_SWITCH if action_frame => {
                    let wbcs = ie::parse_wide_bandwidth_channel_switch(&elements[range])?;
                    csa_builder.add_wide_bandwidth_channel_switch((*wbcs).clone());
                }
                ie::IeType::TRANSMIT_POWER_ENVELOPE if action_frame => {
                    let tpe = ie::parse_transmit_power_envelope(&elements[range])?;
                    csa_builder.add_transmit_power_envelope(tpe);
                }
                _ => (),
            }
        }
        match csa_builder.build() {
            ChannelSwitchResult::ChannelSwitch(cs) => self.handle_channel_switch(cs).await,
            ChannelSwitchResult::NoChannelSwitch => Ok(()),
            ChannelSwitchResult::Error(err) => Err(err.into()),
        }
    }

    async fn handle_channel_switch(
        &mut self,
        channel_switch: ChannelSwitch,
    ) -> Result<(), anyhow::Error> {
        if !channel_switch.compatible() {
            bail!("Incompatible channel switch announcement received.");
        }

        self.actions.disable_scanning().await?;
        if channel_switch.channel_switch_count == 0 {
            self.set_main_channel(channel_switch.new_channel).await.map_err(|e| e.into())
        } else {
            if channel_switch.pause_transmission {
                // TODO(b/254334420): Determine if this should be fatal to the switch.
                self.actions.disable_tx()?;
            }
            let time = self
                .channel_state
                .channel_switch_time_from_count(channel_switch.channel_switch_count);
            let event_id = self.actions.schedule_channel_switch_timeout(time.into());
            self.channel_state.pending_channel_switch.replace((channel_switch, event_id));
            Ok(())
        }
    }

    pub async fn handle_channel_switch_timeout(
        &mut self,
        event_id: EventId,
    ) -> Result<(), anyhow::Error> {
        if let Some((channel_switch, switch_id)) = self.channel_state.pending_channel_switch.take()
        {
            if event_id == switch_id {
                // This is the most recently scheduled channel switch. Execute it.
                self.set_main_channel(channel_switch.new_channel).await?;
            } else {
                self.channel_state.pending_channel_switch.replace((channel_switch, switch_id));
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct ChannelSwitch {
    pub channel_switch_count: u8,
    pub new_channel: fidl_common::WlanChannel,
    pub pause_transmission: bool,
    pub new_operating_class: Option<u8>,
    // TODO(https://fxbug.dev/42180124): Support transmit power envelope.
    pub new_transmit_power_envelope_specified: bool,
}

impl ChannelSwitch {
    // TODO(https://fxbug.dev/42180124): Support channel switch related feature queries.
    /// Determines whether this ChannelSwitch can be performed by the driver.
    fn compatible(&self) -> bool {
        self.new_operating_class.is_none()
            && !self.new_transmit_power_envelope_specified
            && !self.pause_transmission
    }
}

#[derive(Default)]
pub struct ChannelSwitchBuilder<B> {
    channel_switch: Option<ie::ChannelSwitchAnnouncement>,
    secondary_channel_offset: Option<ie::SecChanOffset>,
    extended_channel_switch: Option<ie::ExtendedChannelSwitchAnnouncement>,
    new_country: Option<ie::CountryView<B>>,
    wide_bandwidth_channel_switch: Option<ie::WideBandwidthChannelSwitch>,
    transmit_power_envelope: Option<ie::TransmitPowerEnvelopeView<B>>,
}

#[derive(Debug)]
pub enum ChannelSwitchResult {
    ChannelSwitch(ChannelSwitch),
    NoChannelSwitch,
    Error(ChannelSwitchError),
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelSwitchError {
    #[error("Frame contains multiple channel switch elements with conflicting information.")]
    ConflictingElements,
    #[error("Invalid channel switch mode {}", _0)]
    InvalidChannelSwitchMode(u8),
}

impl<B: SplitByteSlice> ChannelSwitchBuilder<B> {
    // Convert a set of received channel-switch-related IEs into the parameters
    // for a channel switch. Returns an error if the IEs received do not describe
    // a deterministic, valid channel switch.
    pub fn build(self) -> ChannelSwitchResult {
        // Extract shared information from the channel switch or extended channel switch elements
        // present. If both are present we check that they agree on the destination channel and then
        // use the CSA instead of the ECSA. This decision is to avoid specifying a
        // new_operating_class wherever possible, since operating class switches are unsupported.
        let (mode, new_channel_number, channel_switch_count, new_operating_class) =
            if let Some(csa) = self.channel_switch {
                if let Some(ecsa) = self.extended_channel_switch {
                    // If both CSA and ECSA elements are present, make sure they match.
                    if csa.new_channel_number != ecsa.new_channel_number {
                        return ChannelSwitchResult::Error(ChannelSwitchError::ConflictingElements);
                    }
                }
                // IEEE Std 802.11-2016 11.9.8 describes the operation of a CSA.
                (csa.mode, csa.new_channel_number, csa.channel_switch_count, None)
            } else if let Some(ecsa) = self.extended_channel_switch {
                // IEEE Std 802.11-2016 11.10 describes the operation of an extended CSA.
                (
                    ecsa.mode,
                    ecsa.new_channel_number,
                    ecsa.channel_switch_count,
                    Some(ecsa.new_operating_class),
                )
            } else {
                return ChannelSwitchResult::NoChannelSwitch;
            };

        let pause_transmission = match mode {
            1 => true,
            0 => false,
            other => {
                return ChannelSwitchResult::Error(ChannelSwitchError::InvalidChannelSwitchMode(
                    other,
                ))
            }
        };

        // IEEE Std 802.11-2016 9.4.2.159 Table 9-252 specifies that wide bandwidth channel switch
        // elements are treated identically to those in a VHT element.
        let vht_cbw_and_segs = self
            .wide_bandwidth_channel_switch
            .map(|wbcs| (wbcs.new_width, wbcs.new_center_freq_seg0, wbcs.new_center_freq_seg1));
        let sec_chan_offset =
            self.secondary_channel_offset.unwrap_or(ie::SecChanOffset::SECONDARY_NONE);
        let (cbw, secondary80) =
            wlan_common::channel::derive_wide_channel_bandwidth(vht_cbw_and_segs, sec_chan_offset)
                .to_fidl();

        ChannelSwitchResult::ChannelSwitch(ChannelSwitch {
            channel_switch_count: channel_switch_count,
            new_channel: fidl_common::WlanChannel { primary: new_channel_number, cbw, secondary80 },
            pause_transmission,
            new_operating_class,
            new_transmit_power_envelope_specified: self.transmit_power_envelope.is_some(),
        })
    }

    pub fn add_channel_switch_announcement(&mut self, csa: ie::ChannelSwitchAnnouncement) {
        self.channel_switch.replace(csa);
    }

    pub fn add_secondary_channel_offset(&mut self, sco: ie::SecChanOffset) {
        self.secondary_channel_offset.replace(sco);
    }

    pub fn add_extended_channel_switch_announcement(
        &mut self,
        ecsa: ie::ExtendedChannelSwitchAnnouncement,
    ) {
        self.extended_channel_switch.replace(ecsa);
    }

    pub fn add_wide_bandwidth_channel_switch(&mut self, wbcs: ie::WideBandwidthChannelSwitch) {
        self.wide_bandwidth_channel_switch.replace(wbcs);
    }

    pub fn add_transmit_power_envelope(&mut self, tpe: ie::TransmitPowerEnvelopeView<B>) {
        self.transmit_power_envelope.replace(tpe);
    }

    pub fn add_channel_switch_wrapper(&mut self, csw: ie::ChannelSwitchWrapperView<B>) {
        csw.new_country.map(|new_country| self.new_country.replace(new_country));
        csw.new_transmit_power_envelope.map(|tpe| self.add_transmit_power_envelope(tpe));
        csw.wide_bandwidth_channel_switch.map(|wbcs| self.add_wide_bandwidth_channel_switch(*wbcs));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::task::Poll;
    use std::pin::pin;
    use test_case::test_case;
    use wlan_common::assert_variant;
    use wlan_common::mac::CapabilityInfo;
    use zerocopy::IntoBytes;

    const NEW_CHANNEL: u8 = 10;
    const NEW_OPERATING_CLASS: u8 = 20;
    const COUNT: u8 = 30;

    const CHANNEL_SWITCH_ANNOUNCEMENT_HEADER: &[u8] = &[37, 3];

    fn csa(
        mode: u8,
        new_channel_number: u8,
        channel_switch_count: u8,
    ) -> ie::ChannelSwitchAnnouncement {
        ie::ChannelSwitchAnnouncement { mode, new_channel_number, channel_switch_count }
    }

    fn csa_bytes(mode: u8, new_channel_number: u8, channel_switch_count: u8) -> Vec<u8> {
        let mut elements = vec![];
        elements.extend(CHANNEL_SWITCH_ANNOUNCEMENT_HEADER);
        elements.extend(csa(mode, new_channel_number, channel_switch_count).as_bytes());
        elements
    }

    fn ecsa(
        mode: u8,
        new_operating_class: u8,
        new_channel_number: u8,
        channel_switch_count: u8,
    ) -> ie::ExtendedChannelSwitchAnnouncement {
        ie::ExtendedChannelSwitchAnnouncement {
            mode,
            new_operating_class,
            new_channel_number,
            channel_switch_count,
        }
    }

    fn wbcs(seg0: u8, seg1: u8) -> ie::WideBandwidthChannelSwitch {
        ie::WideBandwidthChannelSwitch {
            new_width: ie::VhtChannelBandwidth::CBW_80_160_80P80,
            new_center_freq_seg0: seg0,
            new_center_freq_seg1: seg1,
        }
    }

    #[test_case(Some(NEW_OPERATING_CLASS), false, false ; "when operating class present")]
    #[test_case(None, true, false ; "when new TPE present")]
    #[test_case(Some(NEW_OPERATING_CLASS), true, false ; "when operating class and new TPE present")]
    #[test_case(None, false, true ; "when operating class and new TPE absent")]
    #[fuchsia::test]
    fn channel_switch_compatible(
        new_operating_class: Option<u8>,
        new_transmit_power_envelope_specified: bool,
        expected_compatible: bool,
    ) {
        let channel_switch = ChannelSwitch {
            channel_switch_count: COUNT,
            new_channel: fidl_common::WlanChannel {
                primary: NEW_CHANNEL,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
            pause_transmission: false,
            new_operating_class,
            new_transmit_power_envelope_specified,
        };
        assert_eq!(channel_switch.compatible(), expected_compatible);
    }

    #[test]
    fn empty_builder_returns_no_csa() {
        let builder = ChannelSwitchBuilder::<&[u8]>::default();
        assert_variant!(builder.build(), ChannelSwitchResult::NoChannelSwitch);
    }

    #[test_case(0, false ; "when transmission is not paused")]
    #[test_case(1, true ; "when transmission is paused")]
    #[fuchsia::test]
    fn basic_csa_20mhz(mode: u8, pause_transmission: bool) {
        let mut builder = ChannelSwitchBuilder::<&[u8]>::default();
        builder.add_channel_switch_announcement(csa(mode, NEW_CHANNEL, COUNT));
        let channel_switch =
            assert_variant!(builder.build(), ChannelSwitchResult::ChannelSwitch(cs) => cs);
        let expected_channel_switch = ChannelSwitch {
            channel_switch_count: COUNT,
            new_channel: fidl_common::WlanChannel {
                primary: NEW_CHANNEL,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
            pause_transmission,
            new_operating_class: None,
            new_transmit_power_envelope_specified: false,
        };
        assert_eq!(channel_switch, expected_channel_switch);
    }

    #[test_case(0, false ; "when transmission is not paused")]
    #[test_case(1, true ; "when transmission is paused")]
    #[fuchsia::test]
    fn basic_ecsa_20mhz(mode: u8, pause_transmission: bool) {
        let mut builder = ChannelSwitchBuilder::<&[u8]>::default();
        builder.add_extended_channel_switch_announcement(ecsa(
            mode,
            NEW_OPERATING_CLASS,
            NEW_CHANNEL,
            COUNT,
        ));
        let channel_switch =
            assert_variant!(builder.build(), ChannelSwitchResult::ChannelSwitch(cs) => cs);
        let expected_channel_switch = ChannelSwitch {
            channel_switch_count: COUNT,
            new_channel: fidl_common::WlanChannel {
                primary: NEW_CHANNEL,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
            pause_transmission,
            new_operating_class: Some(NEW_OPERATING_CLASS),
            new_transmit_power_envelope_specified: false,
        };
        assert_eq!(channel_switch, expected_channel_switch);
    }

    #[test]
    fn basic_csa_40mhz() {
        let mut builder = ChannelSwitchBuilder::<&[u8]>::default();
        builder.add_channel_switch_announcement(csa(0, NEW_CHANNEL, COUNT));
        builder.add_secondary_channel_offset(ie::SecChanOffset::SECONDARY_ABOVE);
        let channel_switch =
            assert_variant!(builder.build(), ChannelSwitchResult::ChannelSwitch(cs) => cs);
        let expected_channel_switch = ChannelSwitch {
            channel_switch_count: COUNT,
            new_channel: fidl_common::WlanChannel {
                primary: NEW_CHANNEL,
                cbw: fidl_common::ChannelBandwidth::Cbw40,
                secondary80: 0,
            },
            pause_transmission: false,
            new_operating_class: None,
            new_transmit_power_envelope_specified: false,
        };
        assert_eq!(channel_switch, expected_channel_switch);
    }

    #[test]
    fn basic_csa_80mhz() {
        let mut builder = ChannelSwitchBuilder::<&[u8]>::default();
        builder.add_channel_switch_announcement(csa(0, NEW_CHANNEL, COUNT));
        builder.add_secondary_channel_offset(ie::SecChanOffset::SECONDARY_ABOVE);
        builder.add_wide_bandwidth_channel_switch(wbcs(NEW_CHANNEL + 8, 0));
        let channel_switch =
            assert_variant!(builder.build(), ChannelSwitchResult::ChannelSwitch(cs) => cs);
        let expected_channel_switch = ChannelSwitch {
            channel_switch_count: COUNT,
            new_channel: fidl_common::WlanChannel {
                primary: NEW_CHANNEL,
                cbw: fidl_common::ChannelBandwidth::Cbw80,
                secondary80: 0,
            },
            pause_transmission: false,
            new_operating_class: None,
            new_transmit_power_envelope_specified: false,
        };
        assert_eq!(channel_switch, expected_channel_switch);
    }

    #[test]
    fn basic_csa_160mhz() {
        let mut builder = ChannelSwitchBuilder::<&[u8]>::default();
        builder.add_channel_switch_announcement(csa(0, NEW_CHANNEL, COUNT));
        builder.add_secondary_channel_offset(ie::SecChanOffset::SECONDARY_ABOVE);
        builder.add_wide_bandwidth_channel_switch(wbcs(NEW_CHANNEL + 8, NEW_CHANNEL + 16));
        let channel_switch =
            assert_variant!(builder.build(), ChannelSwitchResult::ChannelSwitch(cs) => cs);
        let expected_channel_switch = ChannelSwitch {
            channel_switch_count: COUNT,
            new_channel: fidl_common::WlanChannel {
                primary: NEW_CHANNEL,
                cbw: fidl_common::ChannelBandwidth::Cbw160,
                secondary80: 0,
            },
            pause_transmission: false,
            new_operating_class: None,
            new_transmit_power_envelope_specified: false,
        };
        assert_eq!(channel_switch, expected_channel_switch);
    }

    #[test]
    fn basic_csa_80p80mhz() {
        let mut builder = ChannelSwitchBuilder::<&[u8]>::default();
        builder.add_channel_switch_announcement(csa(0, NEW_CHANNEL, COUNT));
        builder.add_secondary_channel_offset(ie::SecChanOffset::SECONDARY_ABOVE);
        builder.add_wide_bandwidth_channel_switch(wbcs(NEW_CHANNEL + 8, NEW_CHANNEL + 100));
        let channel_switch =
            assert_variant!(builder.build(), ChannelSwitchResult::ChannelSwitch(cs) => cs);
        let expected_channel_switch = ChannelSwitch {
            channel_switch_count: COUNT,
            new_channel: fidl_common::WlanChannel {
                primary: NEW_CHANNEL,
                cbw: fidl_common::ChannelBandwidth::Cbw80P80,
                secondary80: NEW_CHANNEL + 100,
            },
            pause_transmission: false,
            new_operating_class: None,
            new_transmit_power_envelope_specified: false,
        };
        assert_eq!(channel_switch, expected_channel_switch);
    }

    #[test_case(0, false ; "when transmission is not paused")]
    #[test_case(1, true ; "when transmission is paused")]
    #[fuchsia::test]
    fn mixed_csa_ecsa_20mhz(mode: u8, pause_transmission: bool) {
        let mut builder = ChannelSwitchBuilder::<&[u8]>::default();
        builder.add_channel_switch_announcement(csa(mode, NEW_CHANNEL, COUNT));
        builder.add_extended_channel_switch_announcement(ecsa(
            mode,
            NEW_OPERATING_CLASS,
            NEW_CHANNEL,
            COUNT,
        ));
        let channel_switch =
            assert_variant!(builder.build(), ChannelSwitchResult::ChannelSwitch(cs) => cs);
        let expected_channel_switch = ChannelSwitch {
            channel_switch_count: COUNT,
            new_channel: fidl_common::WlanChannel {
                primary: NEW_CHANNEL,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
            pause_transmission,
            new_operating_class: None,
            new_transmit_power_envelope_specified: false,
        };
        assert_eq!(channel_switch, expected_channel_switch);
    }

    #[test]
    fn mixed_csa_ecsa_mismatch_20mhz() {
        let mut builder = ChannelSwitchBuilder::<&[u8]>::default();
        builder.add_channel_switch_announcement(csa(0, NEW_CHANNEL, COUNT));
        let mut ecsa = ecsa(0, NEW_OPERATING_CLASS, NEW_CHANNEL, COUNT);
        ecsa.new_channel_number += 1;
        builder.add_extended_channel_switch_announcement(ecsa);
        let err = assert_variant!(builder.build(), ChannelSwitchResult::Error(err) => err);
        assert_variant!(err, ChannelSwitchError::ConflictingElements);
    }

    #[test]
    fn basic_csa_invalid_mode_20mhz() {
        let mut builder = ChannelSwitchBuilder::<&[u8]>::default();
        builder.add_channel_switch_announcement(csa(123, NEW_CHANNEL, COUNT));
        let err = assert_variant!(builder.build(), ChannelSwitchResult::Error(err) => err);
        assert_variant!(err, ChannelSwitchError::InvalidChannelSwitchMode(123));
    }

    #[derive(Default)]
    struct MockChannelActions {
        actions: Vec<ChannelAction>,
        event_id_ctr: EventId,
    }

    #[derive(Debug, Copy, Clone)]
    enum ChannelAction {
        SwitchChannel(fidl_common::WlanChannel),
        Timeout(EventId, fasync::MonotonicInstant),
        DisableScanning,
        EnableScanning,
        DisableTx,
        EnableTx,
    }

    impl ChannelActions for &mut MockChannelActions {
        async fn switch_channel(
            &mut self,
            new_main_channel: fidl_common::WlanChannel,
        ) -> Result<(), zx::Status> {
            self.actions.push(ChannelAction::SwitchChannel(new_main_channel));
            Ok(())
        }
        fn schedule_channel_switch_timeout(&mut self, time: zx::MonotonicInstant) -> EventId {
            self.event_id_ctr += 1;
            self.actions.push(ChannelAction::Timeout(self.event_id_ctr, time.into()));
            self.event_id_ctr
        }
        async fn disable_scanning(&mut self) -> Result<(), zx::Status> {
            self.actions.push(ChannelAction::DisableScanning);
            Ok(())
        }
        fn enable_scanning(&mut self) {
            self.actions.push(ChannelAction::EnableScanning);
        }
        fn disable_tx(&mut self) -> Result<(), zx::Status> {
            self.actions.push(ChannelAction::DisableTx);
            Ok(())
        }
        fn enable_tx(&mut self) {
            self.actions.push(ChannelAction::EnableTx);
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn channel_state_ignores_empty_beacon_frame() {
        let mut channel_state = ChannelState::default();
        let mut actions = MockChannelActions::default();
        let header = BeaconHdr::new(TimeUnit(10), CapabilityInfo(0));
        let elements = [];
        channel_state
            .test_bind(&mut actions)
            .handle_beacon(&header, &elements[..])
            .await
            .expect("Failed to handle beacon");

        assert!(actions.actions.is_empty());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn channel_state_handles_immediate_csa_in_beacon_frame() {
        let mut channel_state = ChannelState::default();

        let mut actions = MockChannelActions::default();
        let header = BeaconHdr::new(TimeUnit(10), CapabilityInfo(0));
        let mut elements = vec![];
        elements.extend(csa_bytes(0, NEW_CHANNEL, 0));
        channel_state
            .test_bind(&mut actions)
            .handle_beacon(&header, &elements[..])
            .await
            .expect("Failed to handle beacon");

        assert_eq!(actions.actions.len(), 4);
        assert_variant!(actions.actions[0], ChannelAction::DisableScanning);
        let new_channel =
            assert_variant!(actions.actions[1], ChannelAction::SwitchChannel(chan) => chan);
        assert_eq!(new_channel.primary, NEW_CHANNEL);
        assert_variant!(actions.actions[2], ChannelAction::EnableScanning);
        assert_variant!(actions.actions[3], ChannelAction::EnableTx);
    }

    #[test]
    fn channel_state_handles_delayed_csa_in_beacon_frame() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let mut channel_state = ChannelState::default();
        let bcn_header = BeaconHdr::new(TimeUnit(10), CapabilityInfo(0));
        let mut time = fasync::MonotonicInstant::from_nanos(0);
        exec.set_fake_time(time);
        let mut actions = MockChannelActions::default();

        // First channel switch announcement (count = 2)
        {
            let mut bound_channel_state = channel_state.test_bind(&mut actions);
            let elements = csa_bytes(0, NEW_CHANNEL, 2);
            let fut = bound_channel_state.handle_beacon(&bcn_header, &elements[..]);
            let mut fut = pin!(fut);
            assert_variant!(
                exec.run_until_stalled(&mut fut),
                Poll::Ready(Ok(_)),
                "Failed to handle beacon"
            );
        }
        assert_eq!(actions.actions.len(), 2);
        assert_variant!(actions.actions[0], ChannelAction::DisableScanning);
        let (first_event_id, event_time) =
            assert_variant!(actions.actions[1], ChannelAction::Timeout(eid, time) => (eid, time));
        assert_eq!(event_time, (time + (bcn_header.beacon_interval * 2u16).into()).into());
        actions.actions.clear();

        time += bcn_header.beacon_interval.into();
        exec.set_fake_time(time);

        // Second channel switch announcement (count = 1)
        {
            let mut bound_channel_state = channel_state.test_bind(&mut actions);
            let elements = csa_bytes(0, NEW_CHANNEL, 1);
            let fut = bound_channel_state.handle_beacon(&bcn_header, &elements[..]);
            let mut fut = pin!(fut);
            assert_variant!(
                exec.run_until_stalled(&mut fut),
                Poll::Ready(Ok(_)),
                "Failed to handle beacon"
            );
        }
        assert_eq!(actions.actions.len(), 2);
        assert_variant!(actions.actions[0], ChannelAction::DisableScanning);
        let (second_event_id, event_time) =
            assert_variant!(actions.actions[1], ChannelAction::Timeout(eid, time) => (eid, time));
        assert_eq!(event_time, (time + bcn_header.beacon_interval.into()).into());
        actions.actions.clear();

        time += bcn_header.beacon_interval.into();
        exec.set_fake_time(time);

        // First timeout is ignored.
        {
            let mut bound_channel_state = channel_state.test_bind(&mut actions);
            let fut = bound_channel_state.handle_channel_switch_timeout(first_event_id);
            let mut fut = pin!(fut);
            assert_variant!(
                exec.run_until_stalled(&mut fut),
                Poll::Ready(Ok(_)),
                "Failed to handle channel switch timeout"
            );
        }
        assert!(actions.actions.is_empty());

        // Second timeout results in completion.
        {
            let mut bound_channel_state = channel_state.test_bind(&mut actions);
            let fut = bound_channel_state.handle_channel_switch_timeout(second_event_id);
            let mut fut = pin!(fut);
            assert_variant!(
                exec.run_until_stalled(&mut fut),
                Poll::Ready(Ok(_)),
                "Failed to handle channel switch timeout"
            );
        }
        assert_eq!(actions.actions.len(), 3);
        let new_channel =
            assert_variant!(actions.actions[0], ChannelAction::SwitchChannel(chan) => chan);
        assert_eq!(new_channel.primary, NEW_CHANNEL);
        assert_variant!(actions.actions[1], ChannelAction::EnableScanning);
        assert_variant!(actions.actions[2], ChannelAction::EnableTx);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn channel_state_cannot_pause_tx() {
        let mut channel_state = ChannelState::default();
        let bcn_header = BeaconHdr::new(TimeUnit(10), CapabilityInfo(0));
        let mut actions = MockChannelActions::default();

        channel_state
            .test_bind(&mut actions)
            .handle_beacon(&bcn_header, &csa_bytes(1, NEW_CHANNEL, 2)[..])
            .await
            .expect_err("Shouldn't handle channel switch with tx pause");
        assert_eq!(actions.actions.len(), 0);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn channel_state_cannot_parse_malformed_csa() {
        let mut channel_state = ChannelState::default();
        let bcn_header = BeaconHdr::new(TimeUnit(10), CapabilityInfo(0));
        let mut actions = MockChannelActions::default();

        let mut element = vec![];
        element.extend(CHANNEL_SWITCH_ANNOUNCEMENT_HEADER);
        element.extend(&[10, 0, 0][..]); // Garbage info.
        channel_state
            .test_bind(&mut actions)
            .handle_beacon(&bcn_header, &element[..])
            .await
            .expect_err("Should not handle malformed beacon");
        assert_eq!(actions.actions.len(), 0);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn channel_state_handles_immediate_csa_in_action_frame() {
        let mut channel_state = ChannelState::default();

        let mut actions = MockChannelActions::default();
        channel_state
            .test_bind(&mut actions)
            .handle_announcement_frame(&csa_bytes(0, NEW_CHANNEL, 0)[..])
            .await
            .expect("Failed to handle beacon");

        assert_eq!(actions.actions.len(), 4);
        assert_variant!(actions.actions[0], ChannelAction::DisableScanning);
        let new_channel =
            assert_variant!(actions.actions[1], ChannelAction::SwitchChannel(chan) => chan);
        assert_eq!(new_channel.primary, NEW_CHANNEL);
        assert_variant!(actions.actions[2], ChannelAction::EnableScanning);
        assert_variant!(actions.actions[3], ChannelAction::EnableTx);
    }

    #[test]
    fn channel_state_handles_delayed_csa_in_announcement_frame() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let mut channel_state = ChannelState::default();
        let bcn_header = BeaconHdr::new(TimeUnit(100), CapabilityInfo(0));
        let bcn_time: fasync::MonotonicInstant =
            fasync::MonotonicInstant::from_nanos(0) + bcn_header.beacon_interval.into();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(0));
        let mut actions = MockChannelActions::default();

        // Empty beacon frame to configure beacon parameters.
        {
            let mut bound_channel_state = channel_state.test_bind(&mut actions);
            let elements = [];
            let fut = bound_channel_state.handle_beacon(&bcn_header, &elements[..]);
            let mut fut = pin!(fut);
            assert_variant!(
                exec.run_until_stalled(&mut fut),
                Poll::Ready(Ok(_)),
                "Failed to handle beacon"
            );
        }
        assert!(actions.actions.is_empty());

        // CSA action frame arrives some time between beacons.
        exec.set_fake_time(bcn_time - fasync::MonotonicDuration::from_micros(500));
        {
            let mut bound_channel_state = channel_state.test_bind(&mut actions);
            let elements = csa_bytes(0, NEW_CHANNEL, 1);
            let fut = bound_channel_state.handle_announcement_frame(&elements[..]);
            let mut fut = pin!(fut);
            assert_variant!(
                exec.run_until_stalled(&mut fut),
                Poll::Ready(Ok(_)),
                "Failed to handle announcement"
            );
        }
        assert_eq!(actions.actions.len(), 2);
        assert_variant!(actions.actions[0], ChannelAction::DisableScanning);
        let (event_id, event_time) =
            assert_variant!(actions.actions[1], ChannelAction::Timeout(eid, time) => (eid, time));
        assert_eq!(event_time, bcn_time);
        actions.actions.clear();

        // Timeout arrives.
        exec.set_fake_time(bcn_time);
        {
            let mut bound_channel_state = channel_state.test_bind(&mut actions);
            let fut = bound_channel_state.handle_channel_switch_timeout(event_id);
            let mut fut = pin!(fut);
            assert_variant!(
                exec.run_until_stalled(&mut fut),
                Poll::Ready(Ok(_)),
                "Failed to handle channel switch timeout"
            );
        }
        assert_eq!(actions.actions.len(), 3);
        let new_channel =
            assert_variant!(actions.actions[0], ChannelAction::SwitchChannel(chan) => chan);
        assert_eq!(new_channel.primary, NEW_CHANNEL);
        assert_variant!(actions.actions[1], ChannelAction::EnableScanning);
        assert_variant!(actions.actions[2], ChannelAction::EnableTx);
    }

    #[test]
    fn channel_state_handles_delayed_csa_in_announcement_frame_with_missed_beacon() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let mut channel_state = ChannelState::default();
        let bcn_header = BeaconHdr::new(TimeUnit(100), CapabilityInfo(0));
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(0));
        let mut actions = MockChannelActions::default();

        // Empty beacon frame to configure beacon parameters.
        {
            let mut bound_channel_state = channel_state.test_bind(&mut actions);
            let elements = [];
            let fut = bound_channel_state.handle_beacon(&bcn_header, &elements[..]);
            let mut fut = pin!(fut);
            assert_variant!(
                exec.run_until_stalled(&mut fut),
                Poll::Ready(Ok(_)),
                "Failed to handle beacon"
            );
        }
        assert!(actions.actions.is_empty());

        // Advance time by a bit more than one beacon, simulating a missed frame.
        exec.set_fake_time(
            fasync::MonotonicInstant::from_nanos(0)
                + bcn_header.beacon_interval.into()
                + fasync::MonotonicDuration::from_micros(500),
        );

        // CSA action frame arrives after the missed beacon.
        {
            let mut bound_channel_state = channel_state.test_bind(&mut actions);
            let elements = csa_bytes(0, NEW_CHANNEL, 1);
            let fut = bound_channel_state.handle_announcement_frame(&elements[..]);
            let mut fut = pin!(fut);
            assert_variant!(
                exec.run_until_stalled(&mut fut),
                Poll::Ready(Ok(_)),
                "Failed to handle announcement"
            );
        }
        assert_eq!(actions.actions.len(), 2);
        assert_variant!(actions.actions[0], ChannelAction::DisableScanning);
        let (_event_id, event_time) =
            assert_variant!(actions.actions[1], ChannelAction::Timeout(eid, time) => (eid, time));
        // The CSA should be timed based on our best estimate of the missed beacon.
        assert_eq!(
            event_time,
            fasync::MonotonicInstant::from_nanos(0) + (bcn_header.beacon_interval * 2u16).into()
        );
    }
}
