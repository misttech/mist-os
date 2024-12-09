// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod ap;
pub mod client;
pub mod serve;
#[cfg(test)]
pub mod test_utils;

use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_mlme::{self as fidl_mlme, MlmeEvent};
use futures::channel::mpsc;
use thiserror::Error;
use wlan_common::sink::UnboundedSink;
use wlan_common::timer;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct Config {
    pub wep_supported: bool,
    pub wpa1_supported: bool,
}

impl Config {
    pub fn with_wep(mut self) -> Self {
        self.wep_supported = true;
        self
    }

    pub fn with_wpa1(mut self) -> Self {
        self.wpa1_supported = true;
        self
    }
}

#[derive(Debug)]
pub enum MlmeRequest {
    Scan(fidl_mlme::ScanRequest),
    AuthResponse(fidl_mlme::AuthenticateResponse),
    AssocResponse(fidl_mlme::AssociateResponse),
    Connect(fidl_mlme::ConnectRequest),
    Reconnect(fidl_mlme::ReconnectRequest),
    Roam(fidl_mlme::RoamRequest),
    Deauthenticate(fidl_mlme::DeauthenticateRequest),
    Disassociate(fidl_mlme::DisassociateRequest),
    Eapol(fidl_mlme::EapolRequest),
    SetKeys(fidl_mlme::SetKeysRequest),
    SetCtrlPort(fidl_mlme::SetControlledPortRequest),
    Start(fidl_mlme::StartRequest),
    Stop(fidl_mlme::StopRequest),
    GetIfaceCounterStats(responder::Responder<fidl_mlme::GetIfaceCounterStatsResponse>),
    GetIfaceHistogramStats(responder::Responder<fidl_mlme::GetIfaceHistogramStatsResponse>),
    ListMinstrelPeers(responder::Responder<fidl_mlme::MinstrelListResponse>),
    GetMinstrelStats(
        fidl_mlme::MinstrelStatsRequest,
        responder::Responder<fidl_mlme::MinstrelStatsResponse>,
    ),
    SaeHandshakeResp(fidl_mlme::SaeHandshakeResponse),
    SaeFrameTx(fidl_mlme::SaeFrame),
    WmmStatusReq,
    FinalizeAssociation(fidl_mlme::NegotiatedCapabilities),
    QueryDeviceInfo(responder::Responder<fidl_mlme::DeviceInfo>),
    QueryDiscoverySupport(responder::Responder<fidl_common::DiscoverySupport>),
    QueryMacSublayerSupport(responder::Responder<fidl_common::MacSublayerSupport>),
    QuerySecuritySupport(responder::Responder<fidl_common::SecuritySupport>),
    QuerySpectrumManagementSupport(responder::Responder<fidl_common::SpectrumManagementSupport>),
}

impl MlmeRequest {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Scan(_) => "Scan",
            Self::AuthResponse(_) => "AuthResponse",
            Self::AssocResponse(_) => "AssocResponse",
            Self::Connect(_) => "Connect",
            Self::Reconnect(_) => "Reconnect",
            Self::Roam(_) => "Roam",
            Self::Deauthenticate(_) => "Deauthenticate",
            Self::Disassociate(_) => "Disassociate",
            Self::Eapol(_) => "Eapol",
            Self::SetKeys(_) => "SetKeys",
            Self::SetCtrlPort(_) => "SetCtrlPort",
            Self::Start(_) => "Start",
            Self::Stop(_) => "Stop",
            Self::GetIfaceCounterStats(_) => "GetIfaceCounterStats",
            Self::GetIfaceHistogramStats(_) => "GetIfaceHistogramStats",
            Self::ListMinstrelPeers(_) => "ListMinstrelPeers",
            Self::GetMinstrelStats(_, _) => "GetMinstrelStats",
            Self::SaeHandshakeResp(_) => "SaeHandshakeResp",
            Self::SaeFrameTx(_) => "SaeFrameTx",
            Self::WmmStatusReq => "WmmStatusReq",
            Self::FinalizeAssociation(_) => "FinalizeAssociation",
            Self::QueryDeviceInfo(_) => "QueryDeviceInfo",
            Self::QueryDiscoverySupport(_) => "QueryDiscoverySupport",
            Self::QueryMacSublayerSupport(_) => "QueryMacSublayerSupport",
            Self::QuerySecuritySupport(_) => "QuerySecuritySupport",
            Self::QuerySpectrumManagementSupport(_) => "QuerySpectrumManagementSupport",
        }
    }
}

pub trait Station {
    type Event;

    fn on_mlme_event(&mut self, event: fidl_mlme::MlmeEvent);
    fn on_timeout(&mut self, timed_event: timer::Event<Self::Event>);
}

pub type MlmeStream = mpsc::UnboundedReceiver<MlmeRequest>;
pub type MlmeEventStream = mpsc::UnboundedReceiver<MlmeEvent>;
pub type MlmeSink = UnboundedSink<MlmeRequest>;
pub type MlmeEventSink = UnboundedSink<MlmeEvent>;

pub mod responder {
    use futures::channel::oneshot;

    #[derive(Debug)]
    pub struct Responder<T>(oneshot::Sender<T>);

    impl<T> Responder<T> {
        pub fn new() -> (Self, oneshot::Receiver<T>) {
            let (sender, receiver) = oneshot::channel();
            (Responder(sender), receiver)
        }

        pub fn respond(self, result: T) {
            #[allow(
                clippy::unnecessary_lazy_evaluations,
                reason = "mass allow for https://fxbug.dev/381896734"
            )]
            self.0.send(result).unwrap_or_else(|_| ());
        }
    }
}

/// Safely log MlmeEvents without printing private information.
fn mlme_event_name(event: &MlmeEvent) -> &str {
    match event {
        MlmeEvent::OnScanResult { .. } => "OnScanResult",
        MlmeEvent::OnScanEnd { .. } => "OnScanEnd",
        MlmeEvent::ConnectConf { .. } => "ConnectConf",
        MlmeEvent::RoamConf { .. } => "RoamConf",
        MlmeEvent::RoamStartInd { .. } => "RoamStartInd",
        MlmeEvent::RoamResultInd { .. } => "RoamResultInd",
        MlmeEvent::AuthenticateInd { .. } => "AuthenticateInd",
        MlmeEvent::DeauthenticateConf { .. } => "DeauthenticateConf",
        MlmeEvent::DeauthenticateInd { .. } => "DeauthenticateInd",
        MlmeEvent::AssociateInd { .. } => "AssociateInd",
        MlmeEvent::DisassociateConf { .. } => "DisassociateConf",
        MlmeEvent::DisassociateInd { .. } => "DisassociateInd",
        MlmeEvent::SetKeysConf { .. } => "SetKeysConf",
        MlmeEvent::StartConf { .. } => "StartConf",
        MlmeEvent::StopConf { .. } => "StopConf",
        MlmeEvent::EapolConf { .. } => "EapolConf",
        MlmeEvent::SignalReport { .. } => "SignalReport",
        MlmeEvent::EapolInd { .. } => "EapolInd",
        MlmeEvent::RelayCapturedFrame { .. } => "RelayCapturedFrame",
        MlmeEvent::OnChannelSwitched { .. } => "OnChannelSwitched",
        MlmeEvent::OnPmkAvailable { .. } => "OnPmkAvailable",
        MlmeEvent::OnSaeHandshakeInd { .. } => "OnSaeHandshakeInd",
        MlmeEvent::OnSaeFrameRx { .. } => "OnSaeFrameRx",
        MlmeEvent::OnWmmStatusResp { .. } => "OnWmmStatusResp",
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("scan end while not scanning")]
    ScanEndNotScanning,
    #[error("scan end with wrong txn id")]
    ScanEndWrongTxnId,
    #[error("scan result while not scanning")]
    ScanResultNotScanning,
    #[error("scan result with wrong txn id")]
    ScanResultWrongTxnId,
}
