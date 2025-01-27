// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod link_state;

use crate::client::event::{self, Event};
use crate::client::internal::Context;
use crate::client::protection::{build_protection_ie, Protection, ProtectionIe, SecurityContext};
use crate::client::{
    report_connect_finished, report_roam_finished, AssociationFailure, ClientConfig,
    ClientSmeStatus, ConnectResult, ConnectTransactionEvent, ConnectTransactionSink,
    EstablishRsnaFailure, EstablishRsnaFailureReason, RoamFailure, RoamFailureType, RoamResult,
    ServingApInfo,
};
use crate::{mlme_event_name, MlmeRequest, MlmeSink};
use anyhow::bail;
use fidl_fuchsia_wlan_common_security::Authentication;
use fidl_fuchsia_wlan_mlme::{self as fidl_mlme, MlmeEvent};
use fuchsia_inspect_contrib::inspect_log;
use fuchsia_inspect_contrib::log::InspectBytes;
use ieee80211::{Bssid, MacAddr, MacAddrBytes, Ssid};
use link_state::LinkState;
use log::{error, info, warn};
use wlan_common::bss::BssDescription;
use wlan_common::ie::rsn::cipher;
use wlan_common::ie::rsn::suite_selector::OUI;
use wlan_common::ie::{self};
use wlan_common::security::wep::WepKey;
use wlan_common::security::SecurityAuthenticator;
use wlan_common::timer::EventId;
use wlan_rsn::auth;
use wlan_rsn::rsna::{AuthRejectedReason, AuthStatus, SecAssocUpdate, UpdateSink};
use wlan_statemachine::*;
use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
    fidl_fuchsia_wlan_internal as fidl_internal, fidl_fuchsia_wlan_sme as fidl_sme,
};

/// Timeout for the MLME connect op, which consists of Join, Auth, and Assoc steps.
/// TODO(https://fxbug.dev/42182084): Consider having a single overall connect timeout that is
///                        managed by SME and also includes the EstablishRsna step.
const DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT: u32 = 60; // beacon intervals
/// Maximum number of association attempts we will make without achieving a
/// link up state. We abort and notify of a connection result if we exceed this
/// count.
const MAX_REASSOCIATIONS_WITHOUT_LINK_UP: u32 = 5;

const IDLE_STATE: &str = "IdleState";
const DISCONNECTING_STATE: &str = "DisconnectingState";
const CONNECTING_STATE: &str = "ConnectingState";
const ROAMING_STATE: &str = "RoamingState";
const RSNA_STATE: &str = "EstablishingRsnaState";
const LINK_UP_STATE: &str = "LinkUpState";

#[derive(Debug)]
pub struct ConnectCommand {
    pub bss: Box<BssDescription>,
    pub connect_txn_sink: ConnectTransactionSink,
    pub protection: Protection,
    pub authentication: Authentication,
}

#[derive(Debug)]
pub struct Idle {
    cfg: ClientConfig,
}

#[derive(Debug)]
pub struct Connecting {
    cfg: ClientConfig,
    cmd: ConnectCommand,
    protection_ie: Option<ProtectionIe>,
    reassociation_loop_count: u32,
}

#[derive(Debug)]
pub struct Associated {
    cfg: ClientConfig,
    connect_txn_sink: ConnectTransactionSink,
    latest_ap_state: Box<BssDescription>,
    auth_method: Option<auth::MethodName>,
    last_signal_report_time: zx::MonotonicInstant,
    link_state: LinkState,
    protection_ie: Option<ProtectionIe>,
    // TODO(https://fxbug.dev/42163244): Remove `wmm_param` field when wlanstack telemetry is deprecated.
    wmm_param: Option<ie::WmmParam>,
    last_channel_switch_time: Option<zx::MonotonicInstant>,
    reassociation_loop_count: u32,
    authentication: Authentication,
}

#[derive(Debug)]
pub struct Roaming {
    cfg: ClientConfig,
    cmd: ConnectCommand,
    auth_method: Option<auth::MethodName>,
    protection_ie: Option<ProtectionIe>,
}

#[derive(Debug)]
pub struct Disconnecting {
    cfg: ClientConfig,
    action: PostDisconnectAction,
    timeout_id: EventId,
}

statemachine!(
    #[derive(Debug)]
    pub enum ClientState,
    () => Idle,
    Idle => Connecting,
    Connecting => [Associated, Disconnecting, Idle],
    // Receiving a disassociation ind while Associated causes a transition back to Connecting.
    Associated => [Connecting, Roaming, Disconnecting, Idle],
    Roaming => [Associated, Disconnecting, Idle],
    // We transition directly to Connecting if the disconnect was due to a
    // pending connect.
    Disconnecting => [Connecting, Idle],
);

/// Only one PostDisconnectAction may be selected at a time. This means that
/// in some cases a connect or disconnect might be reported as finished when
/// we're actually still in a disconnecting state, since a different trigger
/// for the disconnect might arrive later and abort the earlier one (e.g. two
/// subsequent disconnect requests). We still process all PostDisconnectAction
/// events, and generally expect these events to be initiated one at a time by
/// wlancfg, so this should not be an issue.
enum PostDisconnectAction {
    ReportConnectFinished { sink: ConnectTransactionSink, result: ConnectResult },
    RespondDisconnect { responder: fidl_sme::ClientSmeDisconnectResponder },
    BeginConnect { cmd: ConnectCommand },
    ReportRoamFinished { sink: ConnectTransactionSink, result: RoamResult },
    None,
}

enum AfterDisconnectState {
    Idle(Idle),
    Connecting(Connecting),
}

impl std::fmt::Debug for PostDisconnectAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.write_str("PostDisconnectAction::")?;
        match self {
            PostDisconnectAction::RespondDisconnect { .. } => f.write_str("RespondDisconnect"),
            PostDisconnectAction::BeginConnect { .. } => f.write_str("BeginConnect"),
            PostDisconnectAction::ReportConnectFinished { .. } => {
                f.write_str("ReportConnectFinished")
            }
            PostDisconnectAction::ReportRoamFinished { .. } => f.write_str("ReportRoamFinished"),
            PostDisconnectAction::None => f.write_str("None"),
        }?;
        Ok(())
    }
}

/// Context surrounding the state change, for Inspect logging
pub enum StateChangeContext {
    Disconnect { msg: String, disconnect_source: fidl_sme::DisconnectSource },
    Connect { msg: String, bssid: Bssid, ssid: Ssid },
    Roam { msg: String, bssid: Bssid },
    Msg(String),
}

trait StateChangeContextExt {
    fn set_msg(&mut self, msg: String);
}

impl StateChangeContextExt for Option<StateChangeContext> {
    fn set_msg(&mut self, msg: String) {
        match self {
            Some(ctx) => match ctx {
                StateChangeContext::Disconnect { msg: ref mut inner, .. } => *inner = msg,
                StateChangeContext::Connect { msg: ref mut inner, .. } => *inner = msg,
                StateChangeContext::Roam { msg: ref mut inner, .. } => *inner = msg,
                StateChangeContext::Msg(inner) => *inner = msg,
            },
            None => {
                *self = Some(StateChangeContext::Msg(msg));
            }
        }
    }
}

impl Idle {
    fn on_connect(
        self,
        cmd: ConnectCommand,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Connecting, Idle> {
        // Derive RSN (for WPA2) or Vendor IEs (for WPA1) or neither(WEP/non-protected).
        let protection_ie = match build_protection_ie(&cmd.protection) {
            Ok(ie) => ie,
            Err(e) => {
                let msg = format!("Failed to build protection IEs: {}", e);
                error!("{}", msg);
                let _ = state_change_ctx.replace(StateChangeContext::Connect {
                    msg,
                    bssid: cmd.bss.bssid,
                    ssid: cmd.bss.ssid,
                });
                return Err(self);
            }
        };
        let (auth_type, sae_password, wep_key) = match &cmd.protection {
            Protection::Rsna(rsna) => match rsna.supplicant.get_auth_cfg() {
                auth::Config::Sae { .. } => (fidl_mlme::AuthenticationTypes::Sae, vec![], None),
                auth::Config::DriverSae { password } => {
                    (fidl_mlme::AuthenticationTypes::Sae, password.clone(), None)
                }
                auth::Config::ComputedPsk(_) => {
                    (fidl_mlme::AuthenticationTypes::OpenSystem, vec![], None)
                }
            },
            Protection::Wep(ref key) => {
                let wep_key = build_wep_set_key_descriptor(cmd.bss.bssid, key);
                inspect_log!(context.inspect.rsn_events.lock(), {
                    derived_key: "WEP",
                    cipher: format!("{:?}", cipher::Cipher::new_dot11(wep_key.cipher_suite_type.into_primitive() as u8)),
                    key_index: wep_key.key_id,
                });
                (fidl_mlme::AuthenticationTypes::SharedKey, vec![], Some(wep_key))
            }
            _ => (fidl_mlme::AuthenticationTypes::OpenSystem, vec![], None),
        };
        let security_ie = match protection_ie.as_ref() {
            Some(ProtectionIe::Rsne(v)) => v.to_vec(),
            Some(ProtectionIe::VendorIes(v)) => v.to_vec(),
            None => vec![],
        };
        context.mlme_sink.send(MlmeRequest::Connect(fidl_mlme::ConnectRequest {
            selected_bss: (*cmd.bss).clone().into(),
            connect_failure_timeout: DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT,
            auth_type,
            sae_password,
            wep_key: wep_key.map(Box::new),
            security_ie,
        }));
        context.att_id += 1;

        let msg = connect_cmd_inspect_summary(&cmd);
        let _ = state_change_ctx.replace(StateChangeContext::Connect {
            msg,
            bssid: cmd.bss.bssid,
            ssid: cmd.bss.ssid.clone(),
        });

        Ok(Connecting { cfg: self.cfg, cmd, protection_ie, reassociation_loop_count: 0 })
    }

    fn on_disconnect_complete(
        self,
        context: &mut Context,
        action: PostDisconnectAction,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> AfterDisconnectState {
        match action {
            PostDisconnectAction::RespondDisconnect { responder } => {
                if let Err(e) = responder.send() {
                    error!("Failed to send disconnect response: {}", e);
                }
                AfterDisconnectState::Idle(self)
            }
            PostDisconnectAction::BeginConnect { cmd } => {
                match self.on_connect(cmd, state_change_ctx, context) {
                    Ok(connecting) => AfterDisconnectState::Connecting(connecting),
                    Err(idle) => AfterDisconnectState::Idle(idle),
                }
            }
            PostDisconnectAction::ReportConnectFinished { mut sink, result } => {
                sink.send_connect_result(result);
                AfterDisconnectState::Idle(self)
            }
            PostDisconnectAction::ReportRoamFinished { mut sink, result } => {
                sink.send_roam_result(result);
                AfterDisconnectState::Idle(self)
            }
            PostDisconnectAction::None => AfterDisconnectState::Idle(self),
        }
    }
}

fn parse_wmm_from_ies(ies: &[u8]) -> Option<ie::WmmParam> {
    let mut wmm_param = None;
    for (id, body) in ie::Reader::new(ies) {
        if id == ie::Id::VENDOR_SPECIFIC {
            if let Ok(ie::VendorIe::WmmParam(wmm_param_body)) = ie::parse_vendor_ie(body) {
                match ie::parse_wmm_param(wmm_param_body) {
                    Ok(param) => wmm_param = Some(*param),
                    Err(e) => {
                        warn!(
                            "Fail parsing IEs for WMM param. Bytes: {:?}. Error: {}",
                            wmm_param_body, e
                        );
                    }
                }
            }
        }
    }
    wmm_param
}

impl Connecting {
    fn on_connect_conf(
        mut self,
        conf: fidl_mlme::ConnectConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Associated, Disconnecting> {
        let auth_method = self.cmd.protection.rsn_auth_method();
        let wmm_param = parse_wmm_from_ies(&conf.association_ies);

        let link_state = match conf.result_code {
            fidl_ieee80211::StatusCode::Success => {
                // TODO(https://fxbug.dev/359842400) Check that peer_sta_address matches request.
                match LinkState::new(self.cmd.protection, context) {
                    Ok(link_state) => link_state,
                    Err(failure_reason) => {
                        let msg = "Connect terminated; failed to initialize LinkState".to_string();
                        error!("{}", msg);
                        state_change_ctx.set_msg(msg);
                        send_deauthenticate_request(&self.cmd.bss.bssid, &context.mlme_sink);
                        let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);
                        return Err(Disconnecting {
                            cfg: self.cfg,
                            action: PostDisconnectAction::ReportConnectFinished {
                                sink: self.cmd.connect_txn_sink,
                                result: EstablishRsnaFailure {
                                    auth_method,
                                    reason: failure_reason,
                                }
                                .into(),
                            },
                            timeout_id,
                        });
                    }
                }
            }
            other => {
                let msg = format!("Connect request failed: {:?}", other);
                warn!("{}", msg);
                state_change_ctx.set_msg(msg);
                send_deauthenticate_request(&self.cmd.bss.bssid, &context.mlme_sink);
                let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);
                return Err(Disconnecting {
                    cfg: self.cfg,
                    action: PostDisconnectAction::ReportConnectFinished {
                        sink: self.cmd.connect_txn_sink,
                        result: ConnectResult::Failed(
                            AssociationFailure {
                                bss_protection: self.cmd.bss.protection(),
                                code: other,
                            }
                            .into(),
                        ),
                    },
                    timeout_id,
                });
            }
        };
        state_change_ctx.set_msg("Connect succeeded".to_string());

        if let LinkState::LinkUp(_) = link_state {
            report_connect_finished(&mut self.cmd.connect_txn_sink, ConnectResult::Success);
            self.reassociation_loop_count = 0;
        }

        Ok(Associated {
            cfg: self.cfg,
            connect_txn_sink: self.cmd.connect_txn_sink,
            auth_method,
            last_signal_report_time: now(),
            latest_ap_state: self.cmd.bss,
            link_state,
            protection_ie: self.protection_ie,
            wmm_param,
            last_channel_switch_time: None,
            reassociation_loop_count: self.reassociation_loop_count,
            authentication: self.cmd.authentication,
        })
    }

    fn on_deauthenticate_ind(
        mut self,
        ind: fidl_mlme::DeauthenticateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Idle {
        let msg = format!("Association request failed due to spurious deauthentication; reason code: {:?}, locally_initiated: {:?}",
                          ind.reason_code, ind.locally_initiated);
        warn!("{}", msg);
        // A deauthenticate_ind means that MLME has already cleared the association,
        // so we go directly to Idle instead of through Disconnecting.
        report_connect_finished(
            &mut self.cmd.connect_txn_sink,
            ConnectResult::Failed(
                AssociationFailure {
                    bss_protection: self.cmd.bss.protection(),
                    code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
                }
                .into(),
            ),
        );
        state_change_ctx.set_msg(msg);
        Idle { cfg: self.cfg }
    }

    fn on_disassociate_ind(
        self,
        ind: fidl_mlme::DisassociateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Disconnecting {
        let msg = format!("Association request failed due to spurious disassociation; reason code: {:?}, locally_initiated: {:?}",
                          ind.reason_code, ind.locally_initiated);
        warn!("{}", msg);
        send_deauthenticate_request(&self.cmd.bss.bssid, &context.mlme_sink);
        let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);
        state_change_ctx.set_msg(msg);
        Disconnecting {
            cfg: self.cfg,
            action: PostDisconnectAction::ReportConnectFinished {
                sink: self.cmd.connect_txn_sink,
                result: ConnectResult::Failed(
                    AssociationFailure {
                        bss_protection: self.cmd.bss.protection(),
                        code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
                    }
                    .into(),
                ),
            },
            timeout_id,
        }
    }

    // Sae management functions

    fn on_sae_handshake_ind(
        &mut self,
        ind: fidl_mlme::SaeHandshakeIndication,
        context: &mut Context,
    ) -> Result<(), anyhow::Error> {
        process_sae_handshake_ind(&mut self.cmd.protection, ind, context)
    }

    fn on_sae_frame_rx(
        &mut self,
        frame: fidl_mlme::SaeFrame,
        context: &mut Context,
    ) -> Result<(), anyhow::Error> {
        process_sae_frame_rx(&mut self.cmd.protection, frame, context)
    }

    fn handle_timeout(
        mut self,
        _event_id: EventId,
        event: Event,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Disconnecting> {
        match process_sae_timeout(&mut self.cmd.protection, self.cmd.bss.bssid, event, context) {
            Ok(()) => Ok(self),
            Err(e) => {
                // An error in handling a timeout means that we may have no way to abort a
                // failed handshake. Drop to idle.
                let msg = format!("failed to handle SAE timeout: {:?}", e);
                error!("{}", msg);
                send_deauthenticate_request(&self.cmd.bss.bssid, &context.mlme_sink);
                let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);
                state_change_ctx.set_msg(msg);
                Err(Disconnecting {
                    cfg: self.cfg,
                    action: PostDisconnectAction::ReportConnectFinished {
                        sink: self.cmd.connect_txn_sink,
                        result: ConnectResult::Failed(
                            AssociationFailure {
                                bss_protection: self.cmd.bss.protection(),
                                code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                            }
                            .into(),
                        ),
                    },
                    timeout_id,
                })
            }
        }
    }

    fn disconnect(mut self, context: &mut Context, action: PostDisconnectAction) -> Disconnecting {
        report_connect_finished(&mut self.cmd.connect_txn_sink, ConnectResult::Canceled);
        send_deauthenticate_request(&self.cmd.bss.bssid, &context.mlme_sink);
        let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);
        Disconnecting { cfg: self.cfg, action, timeout_id }
    }
}

impl Associated {
    fn on_disassociate_ind(
        mut self,
        ind: fidl_mlme::DisassociateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Connecting, Disconnecting> {
        let (mut protection, connected_duration) = self.link_state.disconnect();

        let disconnect_reason = fidl_sme::DisconnectCause {
            mlme_event_name: fidl_sme::DisconnectMlmeEventName::DisassociateIndication,
            reason_code: ind.reason_code,
        };
        let disconnect_source = if ind.locally_initiated {
            fidl_sme::DisconnectSource::Mlme(disconnect_reason)
        } else {
            fidl_sme::DisconnectSource::Ap(disconnect_reason)
        };

        if self.reassociation_loop_count >= MAX_REASSOCIATIONS_WITHOUT_LINK_UP {
            // We have exceeded our retry count. Disconnect.
            let fidl_disconnect_info =
                fidl_sme::DisconnectInfo { is_sme_reconnecting: false, disconnect_source };
            self.connect_txn_sink
                .send(ConnectTransactionEvent::OnDisconnect { info: fidl_disconnect_info });
            let msg = format!(
                "received DisassociateInd msg; reason code {:?}. Too many retries, disconnecting.",
                ind.reason_code
            );
            let _ =
                state_change_ctx.replace(StateChangeContext::Disconnect { msg, disconnect_source });
            send_deauthenticate_request(&self.latest_ap_state.bssid, &context.mlme_sink);
            let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);
            Err(Disconnecting { cfg: self.cfg, action: PostDisconnectAction::None, timeout_id })
        } else {
            if connected_duration.is_some() {
                // Only notify clients of SME if the link was fully established.
                let fidl_disconnect_info =
                    fidl_sme::DisconnectInfo { is_sme_reconnecting: true, disconnect_source };
                self.connect_txn_sink
                    .send(ConnectTransactionEvent::OnDisconnect { info: fidl_disconnect_info });
            }

            let msg = format!("received DisassociateInd msg; reason code {:?}", ind.reason_code);
            let _ = state_change_ctx.replace(match connected_duration {
                Some(_) => StateChangeContext::Disconnect { msg, disconnect_source },
                None => StateChangeContext::Msg(msg),
            });

            // Client is disassociating. The ESS-SA must be kept alive but reset.
            if let Protection::Rsna(rsna) = &mut protection {
                // Reset the state of the ESS-SA and its replay counter to zero per IEEE 802.11-2016 12.7.2.
                rsna.supplicant.reset();
            }

            context.att_id += 1;
            let cmd = ConnectCommand {
                bss: self.latest_ap_state,
                connect_txn_sink: self.connect_txn_sink,
                protection,
                authentication: self.authentication,
            };
            let req = fidl_mlme::ReconnectRequest { peer_sta_address: cmd.bss.bssid.to_array() };
            context.mlme_sink.send(MlmeRequest::Reconnect(req));
            Ok(Connecting {
                cfg: self.cfg,
                cmd,
                protection_ie: self.protection_ie,
                reassociation_loop_count: self.reassociation_loop_count + 1,
            })
        }
    }

    fn on_deauthenticate_ind(
        mut self,
        ind: fidl_mlme::DeauthenticateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Idle {
        let (_, connected_duration) = self.link_state.disconnect();

        let disconnect_reason = fidl_sme::DisconnectCause {
            mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
            reason_code: ind.reason_code,
        };
        let disconnect_source = if ind.locally_initiated {
            fidl_sme::DisconnectSource::Mlme(disconnect_reason)
        } else {
            fidl_sme::DisconnectSource::Ap(disconnect_reason)
        };

        match connected_duration {
            Some(_duration) => {
                let fidl_disconnect_info =
                    fidl_sme::DisconnectInfo { is_sme_reconnecting: false, disconnect_source };
                self.connect_txn_sink
                    .send(ConnectTransactionEvent::OnDisconnect { info: fidl_disconnect_info });
            }
            None => {
                let connect_result = EstablishRsnaFailure {
                    auth_method: self.auth_method,
                    reason: EstablishRsnaFailureReason::InternalError,
                }
                .into();
                report_connect_finished(&mut self.connect_txn_sink, connect_result);
            }
        }

        let _ = state_change_ctx.replace(StateChangeContext::Disconnect {
            msg: format!("received DeauthenticateInd msg; reason code {:?}", ind.reason_code),
            disconnect_source,
        });
        Idle { cfg: self.cfg }
    }

    fn process_link_state_update<U, H>(
        mut self,
        update: U,
        update_handler: H,
        context: &mut Context,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Result<Self, Disconnecting>
    where
        H: Fn(
            LinkState,
            U,
            &BssDescription,
            &mut Option<StateChangeContext>,
            &mut Context,
        ) -> Result<LinkState, EstablishRsnaFailureReason>,
    {
        let was_establishing_rsna = match &self.link_state {
            LinkState::EstablishingRsna(_) => true,
            LinkState::Init(_) | LinkState::LinkUp(_) => false,
        };

        let link_state = match update_handler(
            self.link_state,
            update,
            &self.latest_ap_state,
            state_change_ctx,
            context,
        ) {
            Ok(link_state) => link_state,
            Err(failure_reason) => {
                send_deauthenticate_request(&self.latest_ap_state.bssid, &context.mlme_sink);
                let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);
                report_connect_finished(
                    &mut self.connect_txn_sink,
                    EstablishRsnaFailure { auth_method: self.auth_method, reason: failure_reason }
                        .into(),
                );
                return Err(Disconnecting {
                    cfg: self.cfg,
                    action: PostDisconnectAction::None,
                    timeout_id,
                });
            }
        };

        if let LinkState::LinkUp(_) = link_state {
            if was_establishing_rsna {
                report_connect_finished(&mut self.connect_txn_sink, ConnectResult::Success);
                self.reassociation_loop_count = 0;
            }
        }

        Ok(Self { link_state, ..self })
    }

    fn on_eapol_ind(
        self,
        ind: fidl_mlme::EapolIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Disconnecting> {
        // Ignore unexpected EAPoL frames.
        if !self.latest_ap_state.needs_eapol_exchange() {
            return Ok(self);
        }

        // Reject EAPoL frames from other BSS.
        if &ind.src_addr != self.latest_ap_state.bssid.as_array() {
            let eapol_pdu = &ind.data[..];
            inspect_log!(context.inspect.rsn_events.lock(), {
                rx_eapol_frame: InspectBytes(&eapol_pdu),
                foreign_bssid: MacAddr::from(ind.src_addr).to_string(),
                current_bssid: self.latest_ap_state.bssid.to_string(),
                status: "rejected (foreign BSS)",
            });
            return Ok(self);
        }

        self.process_link_state_update(ind, LinkState::on_eapol_ind, context, state_change_ctx)
    }

    fn on_eapol_conf(
        self,
        resp: fidl_mlme::EapolConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Disconnecting> {
        self.process_link_state_update(resp, LinkState::on_eapol_conf, context, state_change_ctx)
    }

    fn on_set_keys_conf(
        self,
        conf: fidl_mlme::SetKeysConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Disconnecting> {
        self.process_link_state_update(conf, LinkState::on_set_keys_conf, context, state_change_ctx)
    }

    fn on_channel_switched(&mut self, info: fidl_internal::ChannelSwitchInfo) {
        self.connect_txn_sink.send(ConnectTransactionEvent::OnChannelSwitched { info });
        self.latest_ap_state.channel.primary = info.new_channel;
        self.last_channel_switch_time = Some(now());
    }

    fn on_wmm_status_resp(
        &mut self,
        status: zx::sys::zx_status_t,
        resp: fidl_internal::WmmStatusResponse,
    ) {
        if status == zx::sys::ZX_OK {
            let wmm_param = self.wmm_param.get_or_insert_default();
            let mut wmm_info = wmm_param.wmm_info.ap_wmm_info();
            wmm_info.set_uapsd(resp.apsd);
            wmm_param.wmm_info.0 = wmm_info.0;
            update_wmm_ac_param(&mut wmm_param.ac_be_params, &resp.ac_be_params);
            update_wmm_ac_param(&mut wmm_param.ac_bk_params, &resp.ac_bk_params);
            update_wmm_ac_param(&mut wmm_param.ac_vo_params, &resp.ac_vo_params);
            update_wmm_ac_param(&mut wmm_param.ac_vi_params, &resp.ac_vi_params);
        }
    }

    fn handle_timeout(
        mut self,
        event_id: EventId,
        event: Event,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Disconnecting> {
        match self.link_state.handle_timeout(event_id, event, state_change_ctx, context) {
            Ok(link_state) => Ok(Associated { link_state, ..self }),
            Err(failure_reason) => {
                send_deauthenticate_request(&self.latest_ap_state.bssid, &context.mlme_sink);
                let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);
                report_connect_finished(
                    &mut self.connect_txn_sink,
                    EstablishRsnaFailure { auth_method: self.auth_method, reason: failure_reason }
                        .into(),
                );

                Err(Disconnecting { cfg: self.cfg, action: PostDisconnectAction::None, timeout_id })
            }
        }
    }

    fn disconnect(self, context: &mut Context, action: PostDisconnectAction) -> Disconnecting {
        send_deauthenticate_request(&self.latest_ap_state.bssid, &context.mlme_sink);
        let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);
        Disconnecting { cfg: self.cfg, action, timeout_id }
    }
}

// Used when the next state after a roam failure is conditionally determined (e.g. if target needs deauth).
enum AfterRoamFailureState {
    Idle(Idle),
    Disconnecting(Disconnecting),
}

// When a roam attempt starts, SME needs to keep track of where the attempt initiated, in order to
// handle a failure.
enum RoamInitiator {
    RoamStartInd,
    RoamRequest,
}

impl Roaming {
    // Disassociation while roaming requires special handling. Since roaming causes loss of
    // association with the original BSS, we must ignore a disassoc from the original BSS. But a
    // disassociation from the target BSS means that the roam attempt failed, and we should
    // transition to Disconnecting.
    fn on_disassociate_ind(
        self,
        ind: fidl_mlme::DisassociateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Roaming, Disconnecting> {
        let peer_sta_address: Bssid = ind.peer_sta_address.into();
        if peer_sta_address != self.cmd.bss.bssid {
            return Ok(self);
        }

        let disconnect_info = make_roam_disconnect_info(
            fidl_sme::DisconnectMlmeEventName::DisassociateIndication,
            Some(ind.reason_code),
        );

        let failure = RoamFailure {
            failure_type: RoamFailureType::ReassociationFailure,
            selected_bss: Some(*self.cmd.bss.clone()),
            disconnect_info,
            selected_bssid: self.cmd.bss.bssid,
            auth_method: self.auth_method,
            status_code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
            establish_rsna_failure_reason: None,
        };
        let msg = format!("Roam failed due to disassociation, reason_code: {:?}", ind.reason_code);
        Err(Self::to_disconnecting(
            msg,
            failure,
            self.cfg,
            self.cmd.connect_txn_sink,
            state_change_ctx,
            context,
        ))
    }

    // Deauthentication while roaming requires special handling. If the deauth came from the
    // original BSS, we ignore it; but if if deauth came from target BSS, we move to Idle.
    fn on_deauthenticate_ind(
        self,
        ind: fidl_mlme::DeauthenticateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Result<Roaming, Idle> {
        let peer_sta_address: Bssid = ind.peer_sta_address.into();
        if peer_sta_address != self.cmd.bss.bssid {
            return Ok(self);
        }

        let disconnect_info = make_roam_disconnect_info(
            fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
            Some(ind.reason_code),
        );

        let failure = RoamFailure {
            failure_type: RoamFailureType::ReassociationFailure,
            selected_bss: Some(*self.cmd.bss.clone()),
            disconnect_info,
            selected_bssid: self.cmd.bss.bssid,
            auth_method: self.auth_method,
            status_code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
            establish_rsna_failure_reason: None,
        };
        let msg =
            format!("Roam failed due to deauthentication, reason_code: {:?}", ind.reason_code);
        Err(self.to_idle(msg, failure, state_change_ctx))
    }

    fn on_sae_handshake_ind(
        &mut self,
        ind: fidl_mlme::SaeHandshakeIndication,
        context: &mut Context,
    ) -> Result<(), anyhow::Error> {
        process_sae_handshake_ind(&mut self.cmd.protection, ind, context)
    }

    fn on_sae_frame_rx(
        &mut self,
        frame: fidl_mlme::SaeFrame,
        context: &mut Context,
    ) -> Result<(), anyhow::Error> {
        process_sae_frame_rx(&mut self.cmd.protection, frame, context)
    }

    fn handle_timeout(
        mut self,
        _event_id: EventId,
        event: Event,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Idle> {
        match process_sae_timeout(&mut self.cmd.protection, self.cmd.bss.bssid, event, context) {
            Ok(()) => Ok(self),
            Err(e) => {
                // An error in handling a timeout means that we may have no way to abort a failed
                // handshake. Drop to idle.
                let msg = format!("failed to handle SAE timeout: {:?}", e);
                let disconnect_info = make_roam_disconnect_info(
                    fidl_sme::DisconnectMlmeEventName::SaeHandshakeResponse,
                    None,
                );
                // Send ReassociationFailure here, similar to the AssociationFailure returned by SAE
                // timeout handler in connect path.
                let failure = RoamFailure {
                    failure_type: RoamFailureType::ReassociationFailure,
                    selected_bss: Some(*self.cmd.bss.clone()),
                    disconnect_info,
                    selected_bssid: self.cmd.bss.bssid,
                    auth_method: self.auth_method,
                    status_code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                    establish_rsna_failure_reason: None,
                };

                // In the unlikely case that we are authenticated but the last frame was dropped,
                // send a deauth.
                send_deauthenticate_request(&self.cmd.bss.bssid, &context.mlme_sink);

                Err(self.to_idle(msg, failure, state_change_ctx))
            }
        }
    }

    fn to_disconnecting(
        //self,
        msg: String,
        failure: RoamFailure,
        cfg: ClientConfig,
        sink: ConnectTransactionSink,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Disconnecting {
        warn!("{}", msg);
        _ = state_change_ctx.replace(StateChangeContext::Disconnect {
            msg,
            disconnect_source: failure.disconnect_info.disconnect_source,
        });

        send_deauthenticate_request(&failure.selected_bssid, &context.mlme_sink);
        let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);

        Disconnecting {
            cfg,
            action: PostDisconnectAction::ReportRoamFinished { sink, result: failure.into() },
            timeout_id,
        }
    }

    #[allow(clippy::wrong_self_convention, reason = "mass allow for https://fxbug.dev/381896734")]
    fn to_idle(
        mut self,
        msg: String,
        failure: RoamFailure,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Idle {
        warn!("{}", msg);
        _ = state_change_ctx.replace(StateChangeContext::Disconnect {
            msg,
            disconnect_source: failure.disconnect_info.disconnect_source,
        });
        report_roam_finished(&mut self.cmd.connect_txn_sink, failure.into());
        Idle { cfg: self.cfg }
    }
}

impl Disconnecting {
    fn handle_deauthenticate_conf(
        self,
        _conf: fidl_mlme::DeauthenticateConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> AfterDisconnectState {
        Idle { cfg: self.cfg }.on_disconnect_complete(context, self.action, state_change_ctx)
    }

    fn handle_timeout(
        self,
        event_id: EventId,
        event: Event,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, AfterDisconnectState> {
        if let Event::DeauthenticateTimeout(_) = event {
            if event_id == self.timeout_id {
                let msg =
                    "Completing disconnect without confirm due to disconnect timeout".to_string();
                error!("{}", msg);
                state_change_ctx.set_msg(msg);
                return Err(Idle { cfg: self.cfg }.on_disconnect_complete(
                    context,
                    self.action,
                    state_change_ctx,
                ));
            }
        }
        Ok(self)
    }

    fn disconnect(self, action: PostDisconnectAction) -> Disconnecting {
        // We were already disconnecting, so we don't need to initiate another request.
        // Instead, clean up the state from the previous attempt and set the new
        // post-disconnect action.
        match self.action {
            PostDisconnectAction::RespondDisconnect { responder } => {
                if let Err(e) = responder.send() {
                    error!("Failed to send disconnect response: {}", e);
                }
            }
            PostDisconnectAction::ReportConnectFinished { mut sink, result } => {
                report_connect_finished(&mut sink, result);
            }
            PostDisconnectAction::ReportRoamFinished { mut sink, result } => {
                report_roam_finished(&mut sink, result);
            }
            PostDisconnectAction::BeginConnect { mut cmd } => {
                report_connect_finished(&mut cmd.connect_txn_sink, ConnectResult::Canceled);
            }
            PostDisconnectAction::None => (),
        }
        Disconnecting { action, ..self }
    }
}

impl ClientState {
    pub fn new(cfg: ClientConfig) -> Self {
        Self::from(State::new(Idle { cfg }))
    }

    fn state_name(&self) -> &'static str {
        match self {
            Self::Idle(_) => IDLE_STATE,
            Self::Connecting(_) => CONNECTING_STATE,
            Self::Associated(state) => match state.link_state {
                LinkState::EstablishingRsna(_) => RSNA_STATE,
                LinkState::LinkUp(_) => LINK_UP_STATE,
                // LinkState always transition to EstablishingRsna or LinkUp on initialization
                // and never transition back
                #[expect(clippy::unreachable)]
                _ => unreachable!(),
            },
            Self::Roaming(_) => ROAMING_STATE,
            Self::Disconnecting(_) => DISCONNECTING_STATE,
        }
    }

    pub fn on_mlme_event(self, event: MlmeEvent, context: &mut Context) -> Self {
        let start_state = self.state_name();
        let mut state_change_ctx: Option<StateChangeContext> = None;

        let new_state = match self {
            Self::Idle(_) => {
                match event {
                    MlmeEvent::OnWmmStatusResp { .. } => (),
                    MlmeEvent::DeauthenticateConf { resp } => {
                        warn!(
                            "Unexpected MLME message while Idle: {:?} for BSSID {:?}",
                            mlme_event_name(&event),
                            resp.peer_sta_address
                        );
                    }
                    _ => warn!("Unexpected MLME message while Idle: {:?}", mlme_event_name(&event)),
                }
                self
            }
            Self::Connecting(state) => match event {
                MlmeEvent::ConnectConf { resp } => {
                    let (transition, connecting) = state.release_data();
                    match connecting.on_connect_conf(resp, &mut state_change_ctx, context) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(disconnecting) => transition.to(disconnecting).into(),
                    }
                }
                MlmeEvent::DeauthenticateInd { ind } => {
                    let (transition, connecting) = state.release_data();
                    let idle = connecting.on_deauthenticate_ind(ind, &mut state_change_ctx);
                    transition.to(idle).into()
                }
                MlmeEvent::DisassociateInd { ind } => {
                    let (transition, connecting) = state.release_data();
                    let disconnecting =
                        connecting.on_disassociate_ind(ind, &mut state_change_ctx, context);
                    transition.to(disconnecting).into()
                }
                MlmeEvent::OnSaeHandshakeInd { ind } => {
                    let (transition, mut connecting) = state.release_data();
                    if let Err(e) = connecting.on_sae_handshake_ind(ind, context) {
                        error!("Failed to process SaeHandshakeInd: {:?}", e);
                    }
                    transition.to(connecting).into()
                }
                MlmeEvent::OnSaeFrameRx { frame } => {
                    let (transition, mut connecting) = state.release_data();
                    if let Err(e) = connecting.on_sae_frame_rx(frame, context) {
                        error!("Failed to process SaeFrameRx: {:?}", e);
                    }
                    transition.to(connecting).into()
                }
                _ => state.into(),
            },
            Self::Associated(mut state) => match event {
                MlmeEvent::DisassociateInd { ind } => {
                    let (transition, associated) = state.release_data();
                    match associated.on_disassociate_ind(ind, &mut state_change_ctx, context) {
                        Ok(connecting) => transition.to(connecting).into(),
                        Err(disconnecting) => transition.to(disconnecting).into(),
                    }
                }
                MlmeEvent::DeauthenticateInd { ind } => {
                    let (transition, associated) = state.release_data();
                    let idle = associated.on_deauthenticate_ind(ind, &mut state_change_ctx);
                    transition.to(idle).into()
                }
                MlmeEvent::SignalReport { ind } => {
                    if matches!(state.link_state, LinkState::LinkUp(_)) {
                        state
                            .connect_txn_sink
                            .send(ConnectTransactionEvent::OnSignalReport { ind });
                    }
                    state.latest_ap_state.rssi_dbm = ind.rssi_dbm;
                    state.latest_ap_state.snr_db = ind.snr_db;
                    state.last_signal_report_time = now();
                    state.into()
                }
                MlmeEvent::EapolInd { ind } => {
                    let (transition, associated) = state.release_data();
                    match associated.on_eapol_ind(ind, &mut state_change_ctx, context) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(disconnecting) => transition.to(disconnecting).into(),
                    }
                }
                MlmeEvent::EapolConf { resp } => {
                    let (transition, associated) = state.release_data();
                    match associated.on_eapol_conf(resp, &mut state_change_ctx, context) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(disconnecting) => transition.to(disconnecting).into(),
                    }
                }
                MlmeEvent::SetKeysConf { conf } => {
                    let (transition, associated) = state.release_data();
                    match associated.on_set_keys_conf(conf, &mut state_change_ctx, context) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(disconnecting) => transition.to(disconnecting).into(),
                    }
                }
                MlmeEvent::OnChannelSwitched { info } => {
                    state.on_channel_switched(info);
                    state.into()
                }
                MlmeEvent::OnWmmStatusResp { status, resp } => {
                    state.on_wmm_status_resp(status, resp);
                    state.into()
                }
                MlmeEvent::RoamStartInd { ind } => {
                    _ = state_change_ctx.replace(StateChangeContext::Msg(
                        "Fullmac-initiated roam initiated".to_owned(),
                    ));
                    let (transition, associated) = state.release_data();
                    match roam_internal(
                        associated,
                        context,
                        ind.selected_bssid.into(),
                        ind.selected_bss,
                        &mut state_change_ctx,
                        RoamInitiator::RoamStartInd,
                    ) {
                        Ok(roaming) => transition.to(roaming).into(),
                        Err(disconnecting) => transition.to(disconnecting).into(),
                    }
                }
                _ => state.into(),
            },
            Self::Roaming(state) => match event {
                MlmeEvent::OnSaeHandshakeInd { ind } => {
                    let (transition, mut roaming) = state.release_data();
                    if let Err(e) = roaming.on_sae_handshake_ind(ind, context) {
                        error!("Failed to process SaeHandshakeInd: {:?}", e);
                    }
                    transition.to(roaming).into()
                }
                MlmeEvent::OnSaeFrameRx { frame } => {
                    let (transition, mut roaming) = state.release_data();
                    if let Err(e) = roaming.on_sae_frame_rx(frame, context) {
                        error!("Failed to process SaeFrameRx: {:?}", e);
                    }
                    transition.to(roaming).into()
                }
                MlmeEvent::DisassociateInd { ind } => {
                    let (transition, roaming) = state.release_data();
                    match roaming.on_disassociate_ind(ind, &mut state_change_ctx, context) {
                        Ok(roaming) => transition.to(roaming).into(),
                        Err(disconnecting) => transition.to(disconnecting).into(),
                    }
                }
                MlmeEvent::DeauthenticateInd { ind } => {
                    let (transition, roaming) = state.release_data();
                    match roaming.on_deauthenticate_ind(ind, &mut state_change_ctx) {
                        Ok(roaming) => transition.to(roaming).into(),
                        Err(idle) => transition.to(idle).into(),
                    }
                }
                MlmeEvent::RoamConf { conf } => {
                    let (transition, roaming) = state.release_data();

                    match roam_handle_result(
                        roaming,
                        RoamResultFields {
                            selected_bssid: conf.selected_bssid.into(),
                            status_code: conf.status_code,
                            original_association_maintained: conf.original_association_maintained,
                            target_bss_authenticated: conf.target_bss_authenticated,
                            association_ies: conf.association_ies,
                        },
                        context,
                        &mut state_change_ctx,
                        RoamInitiator::RoamRequest,
                    ) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(after_roam_failure_state) => match after_roam_failure_state {
                            AfterRoamFailureState::Disconnecting(disconnecting) => {
                                transition.to(disconnecting).into()
                            }
                            AfterRoamFailureState::Idle(idle) => transition.to(idle).into(),
                        },
                    }
                }
                MlmeEvent::RoamResultInd { ind } => {
                    let (transition, roaming) = state.release_data();

                    match roam_handle_result(
                        roaming,
                        RoamResultFields {
                            selected_bssid: ind.selected_bssid.into(),
                            status_code: ind.status_code,
                            original_association_maintained: ind.original_association_maintained,
                            target_bss_authenticated: ind.target_bss_authenticated,
                            association_ies: ind.association_ies,
                        },
                        context,
                        &mut state_change_ctx,
                        RoamInitiator::RoamStartInd,
                    ) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(after_roam_failure_state) => match after_roam_failure_state {
                            AfterRoamFailureState::Disconnecting(disconnecting) => {
                                transition.to(disconnecting).into()
                            }
                            AfterRoamFailureState::Idle(idle) => transition.to(idle).into(),
                        },
                    }
                }
                _ => state.into(),
            },
            Self::Disconnecting(state) => match event {
                MlmeEvent::DeauthenticateConf { resp } => {
                    let (transition, disconnecting) = state.release_data();
                    match disconnecting.handle_deauthenticate_conf(
                        resp,
                        &mut state_change_ctx,
                        context,
                    ) {
                        AfterDisconnectState::Idle(idle) => transition.to(idle).into(),
                        AfterDisconnectState::Connecting(connecting) => {
                            transition.to(connecting).into()
                        }
                    }
                }
                _ => state.into(),
            },
        };

        log_state_change(start_state, &new_state, state_change_ctx, context);
        new_state
    }

    pub fn handle_timeout(self, event_id: EventId, event: Event, context: &mut Context) -> Self {
        let start_state = self.state_name();
        let mut state_change_ctx: Option<StateChangeContext> = None;

        let new_state = match self {
            Self::Connecting(state) => {
                let (transition, connecting) = state.release_data();
                match connecting.handle_timeout(event_id, event, &mut state_change_ctx, context) {
                    Ok(connecting) => transition.to(connecting).into(),
                    Err(disconnecting) => transition.to(disconnecting).into(),
                }
            }
            Self::Associated(state) => {
                let (transition, associated) = state.release_data();
                match associated.handle_timeout(event_id, event, &mut state_change_ctx, context) {
                    Ok(associated) => transition.to(associated).into(),
                    Err(disconnecting) => transition.to(disconnecting).into(),
                }
            }
            Self::Roaming(state) => {
                let (transition, roaming) = state.release_data();
                match roaming.handle_timeout(event_id, event, &mut state_change_ctx, context) {
                    Ok(roaming) => transition.to(roaming).into(),
                    Err(idle) => transition.to(idle).into(),
                }
            }
            Self::Disconnecting(state) => {
                let (transition, disconnecting) = state.release_data();
                match disconnecting.handle_timeout(event_id, event, &mut state_change_ctx, context)
                {
                    Ok(disconnecting) => transition.to(disconnecting).into(),
                    Err(after_disconnect) => match after_disconnect {
                        AfterDisconnectState::Idle(idle) => transition.to(idle).into(),
                        AfterDisconnectState::Connecting(connecting) => {
                            transition.to(connecting).into()
                        }
                    },
                }
            }
            _ => self,
        };

        log_state_change(start_state, &new_state, state_change_ctx, context);
        new_state
    }

    pub fn connect(self, cmd: ConnectCommand, context: &mut Context) -> Self {
        let start_state = self.state_name();
        let mut state_change_ctx: Option<StateChangeContext> = None;

        let new_state = self.disconnect_internal(
            context,
            PostDisconnectAction::BeginConnect { cmd },
            &mut state_change_ctx,
        );

        log_state_change(start_state, &new_state, state_change_ctx, context);
        new_state
    }

    pub fn roam(self, context: &mut Context, selected_bss: fidl_common::BssDescription) -> Self {
        let start_state = self.state_name();
        let mut state_change_ctx =
            Some(StateChangeContext::Msg("Policy-initiated roam attempt in progress".to_owned()));

        let new_state = match self {
            ClientState::Associated(state) => {
                let (transition, state) = state.release_data();
                match roam_internal(
                    state,
                    context,
                    selected_bss.bssid.into(),
                    selected_bss,
                    &mut state_change_ctx,
                    RoamInitiator::RoamRequest,
                ) {
                    Ok(roaming) => transition.to(roaming).into(),
                    Err(disconnecting) => transition.to(disconnecting).into(),
                }
            }
            // The roam function should only be called from Associated state; these branches are
            // present for logging, but they should be fairly rare. If we see these, it is likely
            // due to a roam request coming in during the short periods when one layer is
            // transitioning (e.g. SME started a disconnect just as Policy sent a roam request).
            ClientState::Connecting(state) => {
                warn!("Requested roam could not be attempted, client is connecting");
                state.into()
            }
            ClientState::Roaming(state) => {
                error!("Overlapping roam request ignored, client is already roaming");
                state.into()
            }
            ClientState::Disconnecting(state) => {
                warn!("Requested roam could not be attempted, client is disconnecting");
                state.into()
            }
            ClientState::Idle(state) => {
                warn!("Requested roam could not be attempted, client is idle");
                state.into()
            }
        };

        log_state_change(start_state, &new_state, state_change_ctx, context);
        new_state
    }

    pub fn disconnect(
        mut self,
        context: &mut Context,
        user_disconnect_reason: fidl_sme::UserDisconnectReason,
        responder: fidl_sme::ClientSmeDisconnectResponder,
    ) -> Self {
        let mut disconnected_from_link_up = false;
        let disconnect_source = fidl_sme::DisconnectSource::User(user_disconnect_reason);
        if let Self::Associated(state) = &mut self {
            if let LinkState::LinkUp(_link_up) = &state.link_state {
                disconnected_from_link_up = true;
                let fidl_disconnect_info =
                    fidl_sme::DisconnectInfo { is_sme_reconnecting: false, disconnect_source };
                state
                    .connect_txn_sink
                    .send(ConnectTransactionEvent::OnDisconnect { info: fidl_disconnect_info });
            }
        }

        let start_state = self.state_name();

        let new_state = self.disconnect_internal(
            context,
            PostDisconnectAction::RespondDisconnect { responder },
            &mut None,
        );

        let msg =
            format!("received disconnect command from user; reason {:?}", user_disconnect_reason);
        let state_change_ctx = Some(if disconnected_from_link_up {
            StateChangeContext::Disconnect { msg, disconnect_source }
        } else {
            StateChangeContext::Msg(msg)
        });
        log_state_change(start_state, &new_state, state_change_ctx, context);
        new_state
    }

    fn disconnect_internal(
        self,
        context: &mut Context,
        action: PostDisconnectAction,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Self {
        match self {
            Self::Idle(state) => {
                let (transition, state) = state.release_data();
                match state.on_disconnect_complete(context, action, state_change_ctx) {
                    AfterDisconnectState::Idle(idle) => transition.to(idle).into(),
                    AfterDisconnectState::Connecting(connecting) => {
                        transition.to(connecting).into()
                    }
                }
            }
            Self::Connecting(state) => {
                let (transition, state) = state.release_data();
                transition.to(state.disconnect(context, action)).into()
            }
            Self::Associated(state) => {
                let (transition, state) = state.release_data();
                transition.to(state.disconnect(context, action)).into()
            }
            Self::Roaming(state) => {
                let (transition, state) = state.release_data();
                transition.to(Idle { cfg: state.cfg }).into()
            }
            Self::Disconnecting(state) => {
                let (transition, state) = state.release_data();
                transition.to(state.disconnect(action)).into()
            }
        }
    }

    // Cancel any connect that is in progress. No-op if client is already idle or connected.
    pub fn cancel_ongoing_connect(self, context: &mut Context) -> Self {
        // Only move to idle if client is not already connected. Technically, SME being in
        // transition state does not necessarily mean that a (manual) connect attempt is
        // in progress (since DisassociateInd moves SME to transition state). However, the
        // main thing we are concerned about is that we don't disconnect from an already
        // connected state until the new connect attempt succeeds in selecting BSS.
        if self.in_transition_state() {
            let mut state_change_ctx = None;
            self.disconnect_internal(context, PostDisconnectAction::None, &mut state_change_ctx)
        } else {
            self
        }
    }

    #[allow(
        clippy::match_like_matches_macro,
        reason = "mass allow for https://fxbug.dev/381896734"
    )]
    fn in_transition_state(&self) -> bool {
        match self {
            Self::Idle(_) => false,
            Self::Associated(state) => match state.link_state {
                LinkState::LinkUp { .. } => false,
                _ => true,
            },
            Self::Roaming(_) => false,
            _ => true,
        }
    }

    pub fn status(&self) -> ClientSmeStatus {
        match self {
            Self::Idle(_) => ClientSmeStatus::Idle,
            Self::Connecting(connecting) => {
                ClientSmeStatus::Connecting(connecting.cmd.bss.ssid.clone())
            }
            Self::Associated(associated) => match associated.link_state {
                LinkState::EstablishingRsna { .. } => {
                    ClientSmeStatus::Connecting(associated.latest_ap_state.ssid.clone())
                }
                LinkState::LinkUp { .. } => {
                    let latest_ap_state = &associated.latest_ap_state;
                    ClientSmeStatus::Connected(ServingApInfo {
                        bssid: latest_ap_state.bssid,
                        ssid: latest_ap_state.ssid.clone(),
                        rssi_dbm: latest_ap_state.rssi_dbm,
                        snr_db: latest_ap_state.snr_db,
                        signal_report_time: associated.last_signal_report_time,
                        channel: latest_ap_state.channel,
                        protection: latest_ap_state.protection(),
                        ht_cap: latest_ap_state.raw_ht_cap(),
                        vht_cap: latest_ap_state.raw_vht_cap(),
                        probe_resp_wsc: latest_ap_state.probe_resp_wsc(),
                        wmm_param: associated.wmm_param,
                    })
                }
                // LinkState always transition to EstablishingRsna or LinkUp on initialization
                // and never transition back
                #[expect(clippy::unreachable)]
                _ => unreachable!(),
            },
            Self::Roaming(roaming) => ClientSmeStatus::Roaming(roaming.cmd.bss.bssid),
            Self::Disconnecting(disconnecting) => match &disconnecting.action {
                PostDisconnectAction::BeginConnect { cmd } => {
                    ClientSmeStatus::Connecting(cmd.bss.ssid.clone())
                }
                _ => ClientSmeStatus::Idle,
            },
        }
    }
}

fn roam_internal(
    state: Associated,
    context: &mut Context,
    selected_bssid: Bssid,
    selected_bss: fidl_common::BssDescription,
    state_change_ctx: &mut Option<StateChangeContext>,
    roam_initiator: RoamInitiator,
) -> Result<Roaming, Disconnecting> {
    // Reassociation is imminent, so consider client disassociated from original BSS. Need a new ESS-SA.
    let (mut orig_bss_protection, _connected_duration) = state.link_state.disconnect();
    if let Protection::Rsna(rsna) = &mut orig_bss_protection {
        // Reset the state of the ESS-SA and its replay counter to zero per IEEE 802.11-2016 12.7.2.
        rsna.supplicant.reset();
    }

    // If a failure occurs in this function, SME will need to know how to log the failure, and where
    // to send a deauth.
    let (mlme_event_name, deauth_addr) = match roam_initiator {
        // RoamStartInd means that auth may have already started with target BSS.
        RoamInitiator::RoamStartInd => {
            (fidl_sme::DisconnectMlmeEventName::RoamStartIndication, &selected_bssid)
        }
        // RoamRequest means that client is only authenticated with the original BSS.
        RoamInitiator::RoamRequest => {
            (fidl_sme::DisconnectMlmeEventName::RoamRequest, &state.latest_ap_state.bssid)
        }
    };

    let selected_bss = match BssDescription::try_from(selected_bss) {
        Ok(selected_bss) => selected_bss,
        Err(error) => {
            error!("Roam cannot proceed due to missing/malformed BSS description: {:?}", error);

            send_deauthenticate_request(deauth_addr, &context.mlme_sink);
            let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);

            let disconnect_info = make_roam_disconnect_info(mlme_event_name, None);

            let failure_type = match roam_initiator {
                RoamInitiator::RoamStartInd => RoamFailureType::RoamStartMalformedFailure,
                RoamInitiator::RoamRequest => RoamFailureType::RoamRequestMalformedFailure,
            };

            return Err(Disconnecting {
                cfg: state.cfg,
                action: PostDisconnectAction::ReportRoamFinished {
                    sink: state.connect_txn_sink,
                    result: RoamFailure {
                        failure_type,
                        selected_bss: None,
                        disconnect_info,
                        selected_bssid,
                        auth_method: state.auth_method,
                        status_code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                        establish_rsna_failure_reason: None,
                    }
                    .into(),
                },
                timeout_id,
            });
        }
    };

    let authentication = state.authentication.clone();
    let selected_bss_protection = match SecurityAuthenticator::try_from(authentication)
        .map_err(From::from)
        .and_then(|authenticator| {
            Protection::try_from(SecurityContext {
                security: &authenticator,
                device: &context.device_info,
                security_support: &context.security_support,
                config: &state.cfg,
                bss: &selected_bss.clone(),
            })
        }) {
        Ok(protection) => protection,
        Err(error) => {
            error!("Failed to configure protection for selected BSS during roam: {:?}", error);

            send_deauthenticate_request(deauth_addr, &context.mlme_sink);
            let timeout_id = context.timer.schedule(event::DeauthenticateTimeout);

            let disconnect_info = make_roam_disconnect_info(mlme_event_name, None);

            return Err(Disconnecting {
                cfg: state.cfg,
                action: PostDisconnectAction::ReportRoamFinished {
                    sink: state.connect_txn_sink,
                    result: RoamFailure {
                        status_code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                        failure_type: RoamFailureType::SelectNetworkFailure,
                        selected_bssid: selected_bss.bssid,
                        disconnect_info,
                        auth_method: state.auth_method,
                        selected_bss: Some(selected_bss),
                        establish_rsna_failure_reason: None,
                    }
                    .into(),
                },
                timeout_id,
            });
        }
    };

    // Policy-initiated roam requires that SME send a roam request to MLME.
    if matches!(roam_initiator, RoamInitiator::RoamRequest) {
        let roam_req = fidl_mlme::RoamRequest { selected_bss: selected_bss.clone().into() };
        context.mlme_sink.send(MlmeRequest::Roam(roam_req));
    }

    _ = state_change_ctx.replace(StateChangeContext::Roam {
        msg: "Roam attempt in progress".to_owned(),
        bssid: selected_bssid,
    });
    Ok(Roaming {
        cfg: state.cfg,
        cmd: ConnectCommand {
            bss: Box::new(selected_bss),
            connect_txn_sink: state.connect_txn_sink,
            protection: selected_bss_protection,
            authentication: state.authentication,
        },
        auth_method: state.auth_method,
        // protection_ie from original connection is preserved.
        protection_ie: state.protection_ie,
    })
}

struct RoamResultFields {
    selected_bssid: Bssid,
    status_code: fidl_ieee80211::StatusCode,
    original_association_maintained: bool,
    target_bss_authenticated: bool,
    association_ies: Vec<u8>,
}

// If the roam attempt succeeded, move into Associated with the selected BSS.
// If the roam attempt failed, return the next state, wrapped in AfterRoamFailureState.
fn roam_handle_result(
    mut state: Roaming,
    result_fields: RoamResultFields,
    context: &mut Context,
    state_change_ctx: &mut Option<StateChangeContext>,
    roam_initiator: RoamInitiator,
) -> Result<Associated, AfterRoamFailureState> {
    if result_fields.original_association_maintained {
        warn!("Roam result claims that device is still associated with original BSS, but Fast BSS Transition is currently unsupported");
    }

    // If a failure occurs in this function, SME will need to know how to log the failure.
    let mlme_event_name = match roam_initiator {
        RoamInitiator::RoamStartInd => fidl_sme::DisconnectMlmeEventName::RoamResultIndication,
        RoamInitiator::RoamRequest => fidl_sme::DisconnectMlmeEventName::RoamConfirmation,
    };

    if result_fields.selected_bssid != state.cmd.bss.bssid {
        let disconnect_info = make_roam_disconnect_info(mlme_event_name, None);
        let failure_type = match roam_initiator {
            RoamInitiator::RoamStartInd => RoamFailureType::RoamResultMalformedFailure,
            RoamInitiator::RoamRequest => RoamFailureType::RoamConfirmationMalformedFailure,
        };
        let failure = RoamFailure {
            failure_type,
            selected_bss: None,
            disconnect_info,
            selected_bssid: state.cmd.bss.bssid,
            auth_method: state.auth_method,
            status_code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            establish_rsna_failure_reason: None,
        };
        return Err(AfterRoamFailureState::Disconnecting(Roaming::to_disconnecting(
            "Roam failed; unexpected BSSID in result".to_owned(),
            failure,
            state.cfg,
            state.cmd.connect_txn_sink,
            state_change_ctx,
            context,
        )));
    }

    #[allow(clippy::clone_on_copy, reason = "mass allow for https://fxbug.dev/381896734")]
    match result_fields.status_code {
        fidl_ieee80211::StatusCode::Success => {
            let wmm_param = parse_wmm_from_ies(&result_fields.association_ies);

            // Get started with link state before going to Associated.
            let link_state = match LinkState::new(state.cmd.protection, context) {
                Ok(link_state) => link_state,
                Err(failure_reason) => {
                    let disconnect_info = make_roam_disconnect_info(mlme_event_name, None);
                    let failure = RoamFailure {
                        failure_type: RoamFailureType::EstablishRsnaFailure,
                        selected_bss: Some(*state.cmd.bss.clone()),
                        disconnect_info,
                        selected_bssid: state.cmd.bss.bssid,
                        auth_method: state.auth_method,
                        status_code: fidl_ieee80211::StatusCode::EstablishRsnaFailure,
                        establish_rsna_failure_reason: Some(failure_reason),
                    };

                    if result_fields.target_bss_authenticated {
                        return Err(AfterRoamFailureState::Disconnecting(
                            Roaming::to_disconnecting(
                                "Roam failed; SME failed to initialize LinkState".to_owned(),
                                failure,
                                state.cfg,
                                state.cmd.connect_txn_sink,
                                state_change_ctx,
                                context,
                            ),
                        ));
                    } else {
                        report_roam_finished(&mut state.cmd.connect_txn_sink, failure.into());
                        return Err(AfterRoamFailureState::Idle(Idle { cfg: state.cfg }));
                    }
                }
            };

            let ssid = state.cmd.bss.ssid.clone();
            _ = state_change_ctx.replace(StateChangeContext::Connect {
                msg: "Fullmac-initiated roam succeeded".to_owned(),
                bssid: state.cmd.bss.bssid.clone(),
                ssid,
            });
            state
                .cmd
                .connect_txn_sink
                .send_roam_result(RoamResult::Success(Box::new(*state.cmd.bss.clone())));
            Ok(Associated {
                cfg: state.cfg,
                connect_txn_sink: state.cmd.connect_txn_sink,
                latest_ap_state: state.cmd.bss,
                auth_method: state.auth_method,
                last_signal_report_time: now(),
                link_state,
                protection_ie: state.protection_ie,
                // TODO(https://fxbug.dev/82654): Remove `wmm_param` field when wlanstack telemetry is deprecated.
                wmm_param,
                last_channel_switch_time: None,
                reassociation_loop_count: 0,
                authentication: state.cmd.authentication,
            })
        }
        // Roam attempt failed.
        _ => {
            let msg = format!("Roam failed, status_code {:?}", result_fields.status_code);
            error!("{}", msg);
            let disconnect_info = make_roam_disconnect_info(mlme_event_name, None);
            let failure = RoamFailure {
                failure_type: RoamFailureType::ReassociationFailure,
                selected_bss: Some(*state.cmd.bss.clone()),
                disconnect_info,
                selected_bssid: state.cmd.bss.bssid,
                auth_method: state.auth_method,
                status_code: result_fields.status_code,
                establish_rsna_failure_reason: None,
            };

            if result_fields.target_bss_authenticated {
                Err(AfterRoamFailureState::Disconnecting(Roaming::to_disconnecting(
                    msg,
                    failure,
                    state.cfg,
                    state.cmd.connect_txn_sink,
                    state_change_ctx,
                    context,
                )))
            } else {
                Err(AfterRoamFailureState::Idle(state.to_idle(msg, failure, state_change_ctx)))
            }
        }
    }
}

fn update_wmm_ac_param(ac_params: &mut ie::WmmAcParams, update: &fidl_internal::WmmAcParams) {
    ac_params.aci_aifsn.set_aifsn(update.aifsn);
    ac_params.aci_aifsn.set_acm(update.acm);
    ac_params.ecw_min_max.set_ecw_min(update.ecw_min);
    ac_params.ecw_min_max.set_ecw_max(update.ecw_max);
    ac_params.txop_limit = update.txop_limit;
}

fn process_sae_updates(updates: UpdateSink, peer_sta_address: MacAddr, context: &mut Context) {
    for update in updates {
        match update {
            SecAssocUpdate::TxSaeFrame(frame) => {
                context.mlme_sink.send(MlmeRequest::SaeFrameTx(frame));
            }
            SecAssocUpdate::SaeAuthStatus(status) => context.mlme_sink.send(
                MlmeRequest::SaeHandshakeResp(fidl_mlme::SaeHandshakeResponse {
                    peer_sta_address: peer_sta_address.to_array(),
                    status_code: match status {
                        AuthStatus::Success => fidl_ieee80211::StatusCode::Success,
                        AuthStatus::Rejected(reason) => match reason {
                            AuthRejectedReason::TooManyRetries => {
                                fidl_ieee80211::StatusCode::RejectedSequenceTimeout
                            }
                            AuthRejectedReason::PmksaExpired | AuthRejectedReason::AuthFailed => {
                                fidl_ieee80211::StatusCode::RefusedReasonUnspecified
                            }
                        },
                        AuthStatus::InternalError => {
                            fidl_ieee80211::StatusCode::RefusedReasonUnspecified
                        }
                    },
                }),
            ),

            SecAssocUpdate::ScheduleSaeTimeout(id) => {
                let _ = context.timer.schedule(event::SaeTimeout(id));
            }
            _ => (),
        }
    }
}

fn process_sae_handshake_ind(
    protection: &mut Protection,
    ind: fidl_mlme::SaeHandshakeIndication,
    context: &mut Context,
) -> Result<(), anyhow::Error> {
    let supplicant = match protection {
        Protection::Rsna(rsna) => &mut rsna.supplicant,
        _ => bail!("Unexpected SAE handshake indication"),
    };

    let mut updates = UpdateSink::default();
    supplicant.on_sae_handshake_ind(&mut updates)?;
    process_sae_updates(updates, MacAddr::from(ind.peer_sta_address), context);
    Ok(())
}

fn process_sae_frame_rx(
    protection: &mut Protection,
    frame: fidl_mlme::SaeFrame,
    context: &mut Context,
) -> Result<(), anyhow::Error> {
    let peer_sta_address = MacAddr::from(frame.peer_sta_address);
    let supplicant = match protection {
        Protection::Rsna(rsna) => &mut rsna.supplicant,
        _ => bail!("Unexpected SAE frame received"),
    };

    let mut updates = UpdateSink::default();
    supplicant.on_sae_frame_rx(&mut updates, frame)?;
    process_sae_updates(updates, peer_sta_address, context);
    Ok(())
}

#[allow(clippy::single_match, reason = "mass allow for https://fxbug.dev/381896734")]
fn process_sae_timeout(
    protection: &mut Protection,
    bssid: Bssid,
    event: Event,
    context: &mut Context,
) -> Result<(), anyhow::Error> {
    match event {
        Event::SaeTimeout(timer) => {
            let supplicant = match protection {
                Protection::Rsna(rsna) => &mut rsna.supplicant,
                // Ignore timeouts if we're not using SAE.
                _ => return Ok(()),
            };

            let mut updates = UpdateSink::default();
            supplicant.on_sae_timeout(&mut updates, timer.0)?;
            process_sae_updates(updates, MacAddr::from(bssid), context);
        }
        _ => (),
    }
    Ok(())
}

fn log_state_change(
    start_state: &str,
    new_state: &ClientState,
    state_change_ctx: Option<StateChangeContext>,
    context: &mut Context,
) {
    if start_state == new_state.state_name() && state_change_ctx.is_none() {
        return;
    }

    match state_change_ctx {
        Some(inner) => match inner {
            StateChangeContext::Disconnect { msg, disconnect_source } => {
                // Only log the disconnect source if an operation had an effect of moving from
                // non-idle state to idle state.
                if start_state != IDLE_STATE {
                    info!(
                        "{} => {}, ctx: `{}`, disconnect_source: {:?}",
                        start_state,
                        new_state.state_name(),
                        msg,
                        disconnect_source,
                    );
                }

                inspect_log!(context.inspect.state_events.lock(), {
                    from: start_state,
                    to: new_state.state_name(),
                    ctx: msg,
                });
            }
            StateChangeContext::Connect { msg, bssid, ssid } => {
                inspect_log!(context.inspect.state_events.lock(), {
                    from: start_state,
                    to: new_state.state_name(),
                    ctx: msg,
                    bssid: bssid.to_string(),
                    ssid: ssid.to_string(),
                });
            }
            StateChangeContext::Roam { msg, bssid } => {
                inspect_log!(context.inspect.state_events.lock(), {
                    from: start_state,
                    to: new_state.state_name(),
                    ctx: msg,
                    bssid: bssid.to_string(),
                });
            }
            StateChangeContext::Msg(msg) => {
                inspect_log!(context.inspect.state_events.lock(), {
                    from: start_state,
                    to: new_state.state_name(),
                    ctx: msg,
                });
            }
        },
        None => {
            inspect_log!(context.inspect.state_events.lock(), {
                from: start_state,
                to: new_state.state_name(),
            });
        }
    }
}

fn build_wep_set_key_descriptor(bssid: Bssid, key: &WepKey) -> fidl_mlme::SetKeyDescriptor {
    let cipher_suite = match key {
        WepKey::Wep40(_) => cipher::WEP_40,
        WepKey::Wep104(_) => cipher::WEP_104,
    };
    fidl_mlme::SetKeyDescriptor {
        key_type: fidl_mlme::KeyType::Pairwise,
        key: key.clone().into(),
        key_id: 0,
        address: bssid.to_array(),
        cipher_suite_oui: OUI.into(),
        cipher_suite_type: fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(
            cipher_suite.into(),
        ),
        rsc: 0,
    }
}

/// Custom logging for ConnectCommand because its normal full debug string is too large, and we
/// want to reduce how much we log in memory for Inspect. Additionally, in the future, we'd need
/// to anonymize information like BSSID and SSID.
fn connect_cmd_inspect_summary(cmd: &ConnectCommand) -> String {
    let bss = &cmd.bss;
    format!(
        "ConnectCmd {{ \
         capability_info: {capability_info:?}, rates: {rates:?}, \
         protected: {protected:?}, channel: {channel}, \
         rssi: {rssi:?}, ht_cap: {ht_cap:?}, ht_op: {ht_op:?}, \
         vht_cap: {vht_cap:?}, vht_op: {vht_op:?} }}",
        capability_info = bss.capability_info,
        rates = bss.rates(),
        protected = bss.rsne().is_some(),
        channel = bss.channel,
        rssi = bss.rssi_dbm,
        ht_cap = bss.ht_cap().is_some(),
        ht_op = bss.ht_op().is_some(),
        vht_cap = bss.vht_cap().is_some(),
        vht_op = bss.vht_op().is_some()
    )
}

fn send_deauthenticate_request(bssid: &Bssid, mlme_sink: &MlmeSink) {
    mlme_sink.send(MlmeRequest::Deauthenticate(fidl_mlme::DeauthenticateRequest {
        peer_sta_address: bssid.to_array(),
        reason_code: fidl_ieee80211::ReasonCode::StaLeaving,
    }));
}

// Returns a DisconnectInfo for given roam failure parameters. reason_code defaults to UnspecifiedReason.
fn make_roam_disconnect_info(
    mlme_event_name: fidl_sme::DisconnectMlmeEventName,
    reason_code: Option<fidl_ieee80211::ReasonCode>,
) -> fidl_sme::DisconnectInfo {
    let reason_code = match reason_code {
        Some(reason_code) => reason_code,
        None => fidl_ieee80211::ReasonCode::UnspecifiedReason,
    };
    let disconnect_reason = fidl_sme::DisconnectCause { mlme_event_name, reason_code };
    let disconnect_source = fidl_sme::DisconnectSource::Mlme(disconnect_reason);
    fidl_sme::DisconnectInfo { is_sme_reconnecting: false, disconnect_source }
}

fn now() -> zx::MonotonicInstant {
    zx::MonotonicInstant::get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::format_err;
    use diagnostics_assertions::{
        assert_data_tree, AnyBytesProperty, AnyNumericProperty, AnyStringProperty,
    };
    use fidl_fuchsia_wlan_common_security::{Credentials, Protocol};
    use fuchsia_async::DurationExt;
    use fuchsia_inspect::Inspector;
    use futures::channel::mpsc;
    use futures::{Stream, StreamExt};
    use link_state::{EstablishingRsna, LinkUp};
    use std::sync::Arc;
    use std::task::Poll;
    use wlan_common::bss::Protection as BssProtection;
    use wlan_common::channel::{Cbw, Channel};
    use wlan_common::ie::fake_ies::{fake_probe_resp_wsc_ie_bytes, get_vendor_ie_bytes_for_wsc_ie};
    use wlan_common::ie::rsn::rsne::Rsne;
    use wlan_common::test_utils::fake_features::{
        fake_mac_sublayer_support, fake_security_support, fake_spectrum_management_support_empty,
    };
    use wlan_common::test_utils::fake_stas::IesOverrides;
    use wlan_common::{assert_variant, fake_bss_description, timer};
    use wlan_rsn::key::exchange::Key;
    use wlan_rsn::rsna::SecAssocStatus;
    use wlan_rsn::NegotiatedProtection;
    use {
        fidl_fuchsia_wlan_common as fidl_common,
        fidl_fuchsia_wlan_common_security as fidl_security, fidl_internal,
    };

    use crate::client::event::RsnaCompletionTimeout;
    use crate::client::rsn::Rsna;
    use crate::client::test_utils::{
        create_connect_conf, create_on_wmm_status_resp, expect_stream_empty, fake_wmm_param,
        mock_psk_supplicant, MockSupplicant, MockSupplicantController,
    };
    use crate::client::{inspect, ConnectTransactionStream, RoamFailureType};
    use crate::test_utils::{self, make_wpa1_ie};
    use crate::MlmeStream;

    #[test]
    fn connect_happy_path_unprotected() {
        let mut h = TestHelper::new();
        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_one();
        let bss = (*command.bss).clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(req))) => {
            assert_eq!(req, fidl_mlme::ConnectRequest {
                selected_bss: bss.clone().into(),
                connect_failure_timeout: DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT,
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
                sae_password: vec![],
                wep_key: None,
                security_ie: vec![],
            });
        });

        // (mlme->sme) Send a ConnectConf as a response
        let connect_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let _state = state.on_mlme_event(connect_conf, &mut h.context);

        // User should be notified that we are connected
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: IDLE_STATE,
                        to: CONNECTING_STATE,
                        bssid: bss.bssid.to_string(),
                        ssid: bss.ssid.to_string(),
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: LINK_UP_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn connect_happy_path_protected() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = (*command.bss).clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(req))) => {
            assert_eq!(req, fidl_mlme::ConnectRequest {
                selected_bss: bss.clone().into(),
                connect_failure_timeout: DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT,
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
                sae_password: vec![],
                wep_key: None,
                security_ie: vec![
                    0x30, 18, // Element header
                    1, 0, // Version
                    0x00, 0x0F, 0xAC, 4, // Group Cipher: CCMP-128
                    1, 0, 0x00, 0x0F, 0xAC, 4, // 1 Pairwise Cipher: CCMP-128
                    1, 0, 0x00, 0x0F, 0xAC, 2, // 1 AKM: PSK
                ],
            });
        });

        // (mlme->sme) Send a ConnectConf as a response
        suppl_mock
            .set_start_updates(vec![SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)]);
        let connect_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(connect_conf, &mut h.context);
        assert!(suppl_mock.is_supplicant_started());

        // (mlme->sme) Send an EapolInd, mock supplicant with key frame
        let update = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: true,
        };
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![update]);

        expect_eapol_req(&mut h.mlme_stream, bss.bssid);

        // (mlme->sme) Send an EapolInd, mock supplicant with keys
        let ptk = SecAssocUpdate::Key(Key::Ptk(test_utils::ptk()));
        let gtk = SecAssocUpdate::Key(Key::Gtk(test_utils::gtk()));
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![ptk, gtk]);

        expect_set_ptk(&mut h.mlme_stream, bss.bssid);
        expect_set_gtk(&mut h.mlme_stream);

        let state = on_set_keys_conf(state, &mut h, vec![0, 2]);

        // (mlme->sme) Send an EapolInd, mock supplicant with completion status
        let update = SecAssocUpdate::Status(SecAssocStatus::EssSaEstablished);
        let _state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![update]);

        expect_set_ctrl_port(&mut h.mlme_stream, bss.bssid, fidl_mlme::ControlledPortState::Open);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: IDLE_STATE,
                        to: CONNECTING_STATE,
                        bssid: bss.bssid.to_string(),
                        ssid: bss.ssid.to_string(),
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: RSNA_STATE,
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: RSNA_STATE,
                        to: LINK_UP_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn connect_happy_path_wpa1() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_wpa1(supplicant);
        let bss = (*command.bss).clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(req))) => {
            assert_eq!(req, fidl_mlme::ConnectRequest {
                selected_bss: bss.clone().into(),
                connect_failure_timeout: DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT,
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
                sae_password: vec![],
                wep_key: None,
                security_ie: vec![
                    0xdd, 0x16, 0x00, 0x50, 0xf2, // IE header
                    0x01, // MSFT specific IE type (WPA)
                    0x01, 0x00, // WPA version
                    0x00, 0x50, 0xf2, 0x02, // multicast cipher: TKIP
                    0x01, 0x00, 0x00, 0x50, 0xf2, 0x02, // 1 unicast cipher
                    0x01, 0x00, 0x00, 0x50, 0xf2, 0x02, // 1 AKM: PSK
                ],
            });
        });

        // (mlme->sme) Send a ConnectConf as a response
        suppl_mock
            .set_start_updates(vec![SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)]);
        let connect_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(connect_conf, &mut h.context);
        assert!(suppl_mock.is_supplicant_started());

        // (mlme->sme) Send an EapolInd, mock supplicant with key frame
        let update = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: false,
        };
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![update]);

        expect_eapol_req(&mut h.mlme_stream, bss.bssid);

        // (mlme->sme) Send an EapolInd, mock supplicant with keys
        let ptk = SecAssocUpdate::Key(Key::Ptk(test_utils::wpa1_ptk()));
        let gtk = SecAssocUpdate::Key(Key::Gtk(test_utils::wpa1_gtk()));
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![ptk, gtk]);

        expect_set_wpa1_ptk(&mut h.mlme_stream, bss.bssid);
        expect_set_wpa1_gtk(&mut h.mlme_stream);

        let state = on_set_keys_conf(state, &mut h, vec![0, 2]);

        // (mlme->sme) Send an EapolInd, mock supplicant with completion status
        let update = SecAssocUpdate::Status(SecAssocStatus::EssSaEstablished);
        let _state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![update]);

        expect_set_ctrl_port(&mut h.mlme_stream, bss.bssid, fidl_mlme::ControlledPortState::Open);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: IDLE_STATE,
                        to: CONNECTING_STATE,
                        bssid: bss.bssid.to_string(),
                        ssid: bss.ssid.to_string(),
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: RSNA_STATE,
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: RSNA_STATE,
                        to: LINK_UP_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn connect_happy_path_wep() {
        let mut h = TestHelper::new();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_wep();
        let bss = (*command.bss).clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(req))) => {
            assert_eq!(req, fidl_mlme::ConnectRequest {
                selected_bss: bss.clone().into(),
                connect_failure_timeout: DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT,
                auth_type: fidl_mlme::AuthenticationTypes::SharedKey,
                sae_password: vec![],
                wep_key: Some(Box::new(fidl_mlme::SetKeyDescriptor {
                    key_type: fidl_mlme::KeyType::Pairwise,
                    key: vec![3; 5],
                    key_id: 0,
                    address: bss.bssid.to_array(),
                    cipher_suite_oui: OUI.into(),
                    cipher_suite_type: fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(1),
                    rsc: 0,
                })),
                security_ie: vec![],
            });
        });

        // (mlme->sme) Send a ConnectConf as a response
        let connect_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let _state = state.on_mlme_event(connect_conf, &mut h.context);

        // User should be notified that we are connected
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: IDLE_STATE,
                        to: CONNECTING_STATE,
                        bssid: bss.bssid.to_string(),
                        ssid: bss.ssid.to_string(),
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: LINK_UP_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn connect_happy_path_wmm() {
        let mut h = TestHelper::new();
        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_one();
        let bss = (*command.bss).clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(_req))));

        // (mlme->sme) Send a ConnectConf as a response
        let connect_conf = fidl_mlme::MlmeEvent::ConnectConf {
            resp: fidl_mlme::ConnectConfirm {
                peer_sta_address: bss.bssid.to_array(),
                result_code: fidl_ieee80211::StatusCode::Success,
                association_id: 42,
                association_ies: vec![
                    0xdd, 0x18, // Vendor IE header
                    0x00, 0x50, 0xf2, 0x02, 0x01, 0x01, // WMM Param header
                    0x80, // Qos Info - U-ASPD enabled
                    0x00, // reserved
                    0x03, 0xa4, 0x00, 0x00, // Best effort AC params
                    0x27, 0xa4, 0x00, 0x00, // Background AC params
                    0x42, 0x43, 0x5e, 0x00, // Video AC params
                    0x62, 0x32, 0x2f, 0x00, // Voice AC params
                ],
            },
        };
        let _state = state.on_mlme_event(connect_conf, &mut h.context);

        // User should be notified that we are connected
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: IDLE_STATE,
                        to: CONNECTING_STATE,
                        bssid: bss.bssid.to_string(),
                        ssid: bss.ssid.to_string(),
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: LINK_UP_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn set_keys_failure() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = (*command.bss).clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(_req))));

        // (mlme->sme) Send a ConnectConf as a response
        suppl_mock
            .set_start_updates(vec![SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)]);
        let connect_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(connect_conf, &mut h.context);
        assert!(suppl_mock.is_supplicant_started());

        // (mlme->sme) Send an EapolInd, mock supplicant with key frame
        let update = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: false,
        };
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![update]);

        expect_eapol_req(&mut h.mlme_stream, bss.bssid);

        // (mlme->sme) Send an EapolInd, mock supplicant with keys
        let ptk = SecAssocUpdate::Key(Key::Ptk(test_utils::ptk()));
        let gtk = SecAssocUpdate::Key(Key::Gtk(test_utils::gtk()));
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![ptk, gtk]);

        expect_set_ptk(&mut h.mlme_stream, bss.bssid);
        expect_set_gtk(&mut h.mlme_stream);

        // (mlme->sme) Send an EapolInd, mock supplicant with completion status
        let update = SecAssocUpdate::Status(SecAssocStatus::EssSaEstablished);
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![update]);

        // No update until all key confs are received.
        assert!(connect_txn_stream.try_next().is_err());

        // One key fails to set
        let state = state.on_mlme_event(
            MlmeEvent::SetKeysConf {
                conf: fidl_mlme::SetKeysConfirm {
                    results: vec![
                        fidl_mlme::SetKeyResult { key_id: 0, status: zx::Status::OK.into_raw() },
                        fidl_mlme::SetKeyResult {
                            key_id: 2,
                            status: zx::Status::INTERNAL.into_raw(),
                        },
                    ],
                },
            },
            &mut h.context,
        );

        assert_variant!(connect_txn_stream.try_next(),
        Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_variant!(result, ConnectResult::Failed(_))
        });

        let state = exchange_deauth(state, &mut h);
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: IDLE_STATE,
                        to: CONNECTING_STATE,
                        bssid: bss.bssid.to_string(),
                        ssid: bss.ssid.to_string(),
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: RSNA_STATE,
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: RSNA_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "3": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn deauth_while_connecting() {
        let mut h = TestHelper::new();
        let (cmd_one, mut connect_txn_stream1) = connect_command_one();
        let bss = cmd_one.bss.clone();
        let bss_protection = bss.protection();
        let state = connecting_state(cmd_one);
        let deauth_ind = MlmeEvent::DeauthenticateInd {
            ind: fidl_mlme::DeauthenticateIndication {
                peer_sta_address: [7, 7, 7, 7, 7, 7],
                reason_code: fidl_ieee80211::ReasonCode::UnspecifiedReason,
                locally_initiated: false,
            },
        };
        let state = state.on_mlme_event(deauth_ind, &mut h.context);
        assert_variant!(connect_txn_stream1.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, AssociationFailure {
                bss_protection,
                code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
            }
            .into());
        });
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn disassoc_while_connecting() {
        let mut h = TestHelper::new();
        let (cmd_one, mut connect_txn_stream1) = connect_command_one();
        let bss = cmd_one.bss.clone();
        let bss_protection = bss.protection();
        let state = connecting_state(cmd_one);
        let disassoc_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [7, 7, 7, 7, 7, 7],
                reason_code: fidl_ieee80211::ReasonCode::PeerkeyMismatch,
                locally_initiated: false,
            },
        };
        let state = state.on_mlme_event(disassoc_ind, &mut h.context);
        let state = exchange_deauth(state, &mut h);
        assert_variant!(connect_txn_stream1.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, AssociationFailure {
                bss_protection,
                code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
            }
            .into());
        });
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn supplicant_fails_to_start_while_connecting() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = command.bss.clone();
        let state = connecting_state(command);

        suppl_mock.set_start_failure(format_err!("failed to start supplicant"));

        // (mlme->sme) Send a ConnectConf
        let assoc_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(assoc_conf, &mut h.context);

        let state = exchange_deauth(state, &mut h);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, EstablishRsnaFailure {
                auth_method: Some(auth::MethodName::Psk),
                reason: EstablishRsnaFailureReason::StartSupplicantFailed,
            }
            .into());
        });
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn bad_eapol_frame_while_establishing_rsna() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = command.bss.clone();
        let state = establishing_rsna_state(command);

        // doesn't matter what we mock here
        let update = SecAssocUpdate::Status(SecAssocStatus::EssSaEstablished);
        suppl_mock.set_on_eapol_frame_updates(vec![update]);

        // (mlme->sme) Send an EapolInd with bad eapol data
        let eapol_ind = create_eapol_ind(bss.bssid, vec![1, 2, 3, 4]);
        let s = state.on_mlme_event(eapol_ind, &mut h.context);

        // There should be no message in the connect_txn_stream
        assert_variant!(connect_txn_stream.try_next(), Err(_));
        assert_variant!(s, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::EstablishingRsna { .. })});

        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {},
                rsn_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        rx_eapol_frame: AnyBytesProperty,
                        status: AnyStringProperty,
                    }
                },
            },
        });
    }

    #[test]
    fn supplicant_fails_to_process_eapol_while_establishing_rsna() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = command.bss.clone();
        let state = establishing_rsna_state(command);

        suppl_mock.set_on_eapol_frame_failure(format_err!("supplicant::on_eapol_frame fails"));

        // (mlme->sme) Send an EapolInd
        let eapol_ind = create_eapol_ind(bss.bssid, test_utils::eapol_key_frame().into());
        let s = state.on_mlme_event(eapol_ind, &mut h.context);

        // There should be no message in the connect_txn_stream
        assert_variant!(connect_txn_stream.try_next(), Err(_));
        assert_variant!(s, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::EstablishingRsna { .. })});

        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {},
                rsn_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        rx_eapol_frame: AnyBytesProperty,
                        status: AnyStringProperty,
                    }
                },
            },
        });
    }

    #[test]
    fn reject_foreign_eapol_frames() {
        let mut h = TestHelper::new();
        let (supplicant, mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = cmd.bss.clone();
        let state = link_up_state(cmd);
        mock.set_on_eapol_frame_callback(|| {
            panic!("eapol frame should not have been processed");
        });

        // Send an EapolInd from foreign BSS.
        let foreign_bssid = Bssid::from([1; 6]);
        let eapol_ind = create_eapol_ind(foreign_bssid, test_utils::eapol_key_frame().into());
        let state = state.on_mlme_event(eapol_ind, &mut h.context);

        // Verify state did not change.
        assert_variant!(state, ClientState::Associated(state) => {
            assert_variant!(
                &state.link_state,
                LinkState::LinkUp(state) => assert_variant!(&state.protection, Protection::Rsna(_))
            )
        });

        assert_data_tree!(h.inspector, root: {
            usme: contains {
                state_events: {},
                rsn_events:  {
                    "0" : {
                        "@time": AnyNumericProperty,
                        rx_eapol_frame: AnyBytesProperty,
                        foreign_bssid: foreign_bssid.to_string(),
                        current_bssid: bss.bssid.to_string(),
                        status: "rejected (foreign BSS)"
                    }
                }
            }
        });
    }

    #[test]
    fn wrong_password_while_establishing_rsna() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = command.bss.clone();
        let state = establishing_rsna_state(command);

        // (mlme->sme) Send an EapolInd, mock supplicant with wrong password status
        let update = SecAssocUpdate::Status(SecAssocStatus::WrongPassword);
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![update]);

        expect_deauth_req(&mut h.mlme_stream, bss.bssid, fidl_ieee80211::ReasonCode::StaLeaving);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, EstablishRsnaFailure {
                auth_method: Some(auth::MethodName::Psk),
                reason: EstablishRsnaFailureReason::InternalError,
            }
            .into());
        });

        // (mlme->sme) Send a DeauthenticateConf as a response
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: bss.bssid.to_array() },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: RSNA_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    fn expect_next_event_at_deadline<E: std::fmt::Debug>(
        executor: &mut fuchsia_async::TestExecutor,
        mut timed_event_stream: impl Stream<Item = timer::Event<E>> + std::marker::Unpin,
        deadline: fuchsia_async::MonotonicInstant,
    ) -> timer::Event<E> {
        assert_variant!(executor.run_until_stalled(&mut timed_event_stream.next()), Poll::Pending);
        assert_eq!(deadline, executor.wake_next_timer().expect("expected pending timer"));
        executor.set_fake_time(deadline);
        assert_variant!(
            executor.run_until_stalled(&mut timed_event_stream.next()),
            Poll::Ready(Some(timed_event)) => timed_event
        )
    }

    #[test]
    fn simple_rsna_response_timeout_with_unresponsive_ap() {
        let mut h = TestHelper::new_with_fake_time();
        h.executor.set_fake_time(fuchsia_async::MonotonicInstant::from_nanos(0));
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = command.bss.clone();

        // Start in an "Connecting" state
        let state = ClientState::from(testing::new_state(Connecting {
            cfg: ClientConfig::default(),
            cmd: command,
            protection_ie: None,
            reassociation_loop_count: 0,
        }));
        suppl_mock
            .set_start_updates(vec![SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)]);
        let assoc_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let rsna_response_deadline =
            zx::MonotonicDuration::from_millis(event::RSNA_RESPONSE_TIMEOUT_MILLIS).after_now();
        let state = state.on_mlme_event(assoc_conf, &mut h.context);
        assert!(suppl_mock.is_supplicant_started());

        // Advance to the response timeout and setup a failure reason
        let mut timed_event_stream = timer::make_async_timed_event_stream(h.time_stream);
        let timed_event = expect_next_event_at_deadline(
            &mut h.executor,
            &mut timed_event_stream,
            rsna_response_deadline,
        );
        assert_variant!(timed_event.event, Event::RsnaResponseTimeout(..));
        suppl_mock.set_on_rsna_response_timeout(EstablishRsnaFailureReason::RsnaResponseTimeout(
            wlan_rsn::Error::EapolHandshakeNotStarted,
        ));
        let state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);

        // Check that SME sends a deauthenticate request and fails the connection
        expect_deauth_req(&mut h.mlme_stream, bss.bssid, fidl_ieee80211::ReasonCode::StaLeaving);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, EstablishRsnaFailure {
                auth_method: Some(auth::MethodName::Psk),
                reason: EstablishRsnaFailureReason::RsnaResponseTimeout(wlan_rsn::Error::EapolHandshakeNotStarted),
            }.into());
        });

        // (mlme->sme) Send a DeauthenticateConf as a response
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: bss.bssid.to_array() },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: RSNA_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: RSNA_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn simple_retransmission_timeout_with_responsive_ap() {
        let mut h = TestHelper::new_with_fake_time();
        h.executor.set_fake_time(fuchsia_async::MonotonicInstant::from_nanos(0));

        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, _connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = command.bss.bssid;

        // Start in an "Connecting" state
        let state = ClientState::from(testing::new_state(Connecting {
            cfg: ClientConfig::default(),
            cmd: command,
            protection_ie: None,
            reassociation_loop_count: 0,
        }));
        suppl_mock
            .set_start_updates(vec![SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)]);
        let assoc_conf = create_connect_conf(bssid, fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(assoc_conf, &mut h.context);
        assert!(suppl_mock.is_supplicant_started());

        let mut timed_event_stream = timer::make_async_timed_event_stream(h.time_stream);

        // Setup mock response to transmit an EAPOL frame upon receipt of an EAPOL frame.
        let tx_eapol_frame_update_sink = vec![SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: true,
        }];
        suppl_mock.set_on_eapol_frame_updates(tx_eapol_frame_update_sink.clone());

        // Send an initial EAPOL frame to SME
        let eapol_ind = MlmeEvent::EapolInd {
            ind: fidl_mlme::EapolIndication {
                src_addr: bssid.to_array(),
                dst_addr: fake_device_info().sta_addr,
                data: test_utils::eapol_key_frame().into(),
            },
        };
        let mut state = state.on_mlme_event(eapol_ind, &mut h.context);

        // Cycle through the RSNA retransmissions and retransmission timeouts
        let mock_number_of_retransmissions = 5;
        for i in 0..=mock_number_of_retransmissions {
            expect_eapol_req(&mut h.mlme_stream, bssid);
            expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

            let rsna_retransmission_deadline =
                zx::MonotonicDuration::from_millis(event::RSNA_RETRANSMISSION_TIMEOUT_MILLIS)
                    .after_now();
            let timed_event = expect_next_event_at_deadline(
                &mut h.executor,
                &mut timed_event_stream,
                rsna_retransmission_deadline,
            );
            assert_variant!(timed_event.event, Event::RsnaRetransmissionTimeout(_));

            if i < mock_number_of_retransmissions {
                suppl_mock
                    .set_on_rsna_retransmission_timeout_updates(tx_eapol_frame_update_sink.clone());
            }
            state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
        }

        // Check that the connection does not fail
        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: RSNA_STATE,
                    },
                },
                rsn_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        "rsna_status": "PmkSaEstablished"
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        "rx_eapol_frame": AnyBytesProperty,
                        "status": "processed",
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "3": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "4": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "5": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "6": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "7": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                }
            },
        });
    }

    #[test]
    fn retransmission_timeouts_do_not_extend_response_timeout() {
        let mut h = TestHelper::new_with_fake_time();
        h.executor.set_fake_time(fuchsia_async::MonotonicInstant::from_nanos(0));

        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = command.bss.clone();

        // Start in an "Connecting" state
        let state = ClientState::from(testing::new_state(Connecting {
            cfg: ClientConfig::default(),
            cmd: command,
            protection_ie: None,
            reassociation_loop_count: 0,
        }));
        suppl_mock
            .set_start_updates(vec![SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)]);
        let assoc_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(assoc_conf, &mut h.context);
        assert!(suppl_mock.is_supplicant_started());

        let mut timed_event_stream = timer::make_async_timed_event_stream(h.time_stream);

        // Setup mock response to transmit an EAPOL frame upon receipt of an EAPOL frame.
        let tx_eapol_frame_update_sink = vec![SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: true,
        }];
        suppl_mock.set_on_eapol_frame_updates(tx_eapol_frame_update_sink.clone());

        // Send an initial EAPOL frame to SME
        let eapol_ind = MlmeEvent::EapolInd {
            ind: fidl_mlme::EapolIndication {
                src_addr: bss.bssid.to_array(),
                dst_addr: fake_device_info().sta_addr,
                data: test_utils::eapol_key_frame().into(),
            },
        };
        // Progress time to avoid scheduling two concurrent rsna response timeouts.
        // TODO(https://fxbug.dev/371613444): Remove when our timer implementation supports better cancel behavior.
        let first_rsna_response_deadline =
            zx::MonotonicDuration::from_millis(event::RSNA_RESPONSE_TIMEOUT_MILLIS).after_now();
        h.executor.set_fake_time(zx::MonotonicDuration::from_millis(1).after_now());
        let mut state = state.on_mlme_event(eapol_ind, &mut h.context);

        // Cycle through the RSNA retransmission timeouts
        let mock_number_of_retransmissions = 5;
        let second_rsna_response_deadline =
            zx::MonotonicDuration::from_millis(event::RSNA_RESPONSE_TIMEOUT_MILLIS).after_now();
        for i in 0..=mock_number_of_retransmissions {
            expect_eapol_req(&mut h.mlme_stream, bss.bssid);
            expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

            let rsna_retransmission_deadline =
                zx::MonotonicDuration::from_millis(event::RSNA_RETRANSMISSION_TIMEOUT_MILLIS)
                    .after_now();
            let timed_event = expect_next_event_at_deadline(
                &mut h.executor,
                &mut timed_event_stream,
                rsna_retransmission_deadline,
            );
            assert_variant!(timed_event.event, Event::RsnaRetransmissionTimeout(_));

            if i < mock_number_of_retransmissions {
                suppl_mock
                    .set_on_rsna_retransmission_timeout_updates(tx_eapol_frame_update_sink.clone());
            }
            state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
        }

        // Expire the first RSNA response timeout. This should do nothing.
        let first_timeout = expect_next_event_at_deadline(
            &mut h.executor,
            &mut timed_event_stream,
            first_rsna_response_deadline,
        );
        assert_variant!(first_timeout.event, Event::RsnaResponseTimeout(..));
        let state = state.handle_timeout(first_timeout.id, first_timeout.event, &mut h.context);
        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

        // Expire the second RSNA response timeout
        let second_timeout = expect_next_event_at_deadline(
            &mut h.executor,
            &mut timed_event_stream,
            second_rsna_response_deadline,
        );
        assert_variant!(second_timeout.event, Event::RsnaResponseTimeout(..));
        suppl_mock.set_on_rsna_response_timeout(EstablishRsnaFailureReason::RsnaResponseTimeout(
            wlan_rsn::Error::EapolHandshakeIncomplete("PTKSA never initialized".to_string()),
        ));
        let state = state.handle_timeout(second_timeout.id, second_timeout.event, &mut h.context);

        expect_deauth_req(&mut h.mlme_stream, bss.bssid, fidl_ieee80211::ReasonCode::StaLeaving);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, EstablishRsnaFailure {
                auth_method: Some(auth::MethodName::Psk),
                reason: EstablishRsnaFailureReason::RsnaResponseTimeout(wlan_rsn::Error::EapolHandshakeIncomplete("PTKSA never initialized".to_string())),
            }.into());
        });

        // (mlme->sme) Send a DeauthenticateConf as a response
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: bss.bssid.to_array() },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: RSNA_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: RSNA_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
                rsn_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        "rsna_status": "PmkSaEstablished"
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        "rx_eapol_frame": AnyBytesProperty,
                        "status": "processed",
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "3": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "4": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "5": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "6": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "7": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    }
                }
            },
        });
    }

    #[test]
    fn simple_completion_timeout_with_responsive_ap() {
        let mut h = TestHelper::new_with_fake_time();
        h.executor.set_fake_time(fuchsia_async::MonotonicInstant::from_nanos(0));

        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = command.bss.clone();

        // Start in an "Connecting" state
        let state = ClientState::from(testing::new_state(Connecting {
            cfg: ClientConfig::default(),
            cmd: command,
            protection_ie: None,
            reassociation_loop_count: 0,
        }));
        suppl_mock
            .set_start_updates(vec![SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)]);
        let assoc_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(assoc_conf, &mut h.context);
        assert!(suppl_mock.is_supplicant_started());
        let rsna_completion_deadline =
            zx::MonotonicDuration::from_millis(event::RSNA_COMPLETION_TIMEOUT_MILLIS).after_now();
        let mut initial_rsna_response_deadline_in_effect = Some(
            zx::MonotonicDuration::from_millis(event::RSNA_RESPONSE_TIMEOUT_MILLIS).after_now(),
        );

        let mut timed_event_stream = timer::make_async_timed_event_stream(h.time_stream);

        // Setup mock response to transmit an EAPOL frame upon receipt of an EAPOL frame
        let tx_eapol_frame_update_sink = vec![SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: true,
        }];
        suppl_mock.set_on_eapol_frame_updates(tx_eapol_frame_update_sink.clone());

        // Send an initial EAPOL frame to SME. Advance the time to prevent scheduling
        // simultaneous response timeouts for simplicity.
        h.executor.set_fake_time(fuchsia_async::MonotonicInstant::from_nanos(100));
        let eapol_ind = fidl_mlme::EapolIndication {
            src_addr: bss.bssid.to_array(),
            dst_addr: fake_device_info().sta_addr,
            data: test_utils::eapol_key_frame().into(),
        };
        let mut state =
            state.on_mlme_event(MlmeEvent::EapolInd { ind: eapol_ind.clone() }, &mut h.context);
        expect_eapol_req(&mut h.mlme_stream, bss.bssid);
        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

        let mut rsna_retransmission_deadline =
            zx::MonotonicDuration::from_millis(event::RSNA_RETRANSMISSION_TIMEOUT_MILLIS)
                .after_now();
        let mut rsna_response_deadline =
            zx::MonotonicDuration::from_millis(event::RSNA_RESPONSE_TIMEOUT_MILLIS).after_now();

        let mock_just_before_progress_frames = 2;
        let just_before_duration = zx::MonotonicDuration::from_nanos(1);
        for _ in 0..mock_just_before_progress_frames {
            // Expire the restransmission timeout
            let timed_event = expect_next_event_at_deadline(
                &mut h.executor,
                &mut timed_event_stream,
                rsna_retransmission_deadline,
            );
            assert_variant!(timed_event.event, Event::RsnaRetransmissionTimeout(_));
            state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
            expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

            // Receive a frame just before the response timeout would have expired.
            if let Some(initial_rsna_response_deadline) = initial_rsna_response_deadline_in_effect {
                h.executor.set_fake_time(initial_rsna_response_deadline - just_before_duration);
            } else {
                h.executor.set_fake_time(rsna_response_deadline - just_before_duration);
            }
            assert!(!h.executor.wake_expired_timers());
            // Setup mock response to transmit another EAPOL frame
            suppl_mock.set_on_eapol_frame_updates(tx_eapol_frame_update_sink.clone());
            state =
                state.on_mlme_event(MlmeEvent::EapolInd { ind: eapol_ind.clone() }, &mut h.context);
            expect_eapol_req(&mut h.mlme_stream, bss.bssid);
            expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");
            rsna_retransmission_deadline =
                zx::MonotonicDuration::from_millis(event::RSNA_RETRANSMISSION_TIMEOUT_MILLIS)
                    .after_now();
            let prev_rsna_response_deadline = rsna_response_deadline;
            rsna_response_deadline =
                zx::MonotonicDuration::from_millis(event::RSNA_RESPONSE_TIMEOUT_MILLIS).after_now();

            // Expire the initial response timeout
            if let Some(initial_rsna_response_deadline) =
                initial_rsna_response_deadline_in_effect.take()
            {
                let timed_event = expect_next_event_at_deadline(
                    &mut h.executor,
                    &mut timed_event_stream,
                    initial_rsna_response_deadline,
                );
                assert_variant!(timed_event.event, Event::RsnaResponseTimeout(..));
                state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
                expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");
            }

            // Expire the response timeout scheduled after receipt of a frame
            let timed_event = expect_next_event_at_deadline(
                &mut h.executor,
                &mut timed_event_stream,
                prev_rsna_response_deadline,
            );
            assert_variant!(timed_event.event, Event::RsnaResponseTimeout(..));
            state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
            expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");
        }

        // Expire the final retransmission timeout
        let timed_event = expect_next_event_at_deadline(
            &mut h.executor,
            &mut timed_event_stream,
            rsna_retransmission_deadline,
        );
        assert_variant!(timed_event.event, Event::RsnaRetransmissionTimeout(_));
        state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

        // Advance to the completion timeout and setup a failure reason
        let timed_event = expect_next_event_at_deadline(
            &mut h.executor,
            &mut timed_event_stream,
            rsna_completion_deadline,
        );
        assert_variant!(timed_event.event, Event::RsnaCompletionTimeout(RsnaCompletionTimeout {}));
        suppl_mock.set_on_rsna_completion_timeout(
            EstablishRsnaFailureReason::RsnaCompletionTimeout(
                wlan_rsn::Error::EapolHandshakeIncomplete("PTKSA never initialized".to_string()),
            ),
        );
        let state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);

        // Check that SME sends a deauthenticate request and fails the connection
        expect_deauth_req(&mut h.mlme_stream, bss.bssid, fidl_ieee80211::ReasonCode::StaLeaving);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, EstablishRsnaFailure {
                auth_method: Some(auth::MethodName::Psk),
                reason: EstablishRsnaFailureReason::RsnaCompletionTimeout(wlan_rsn::Error::EapolHandshakeIncomplete("PTKSA never initialized".to_string())),
            }.into());
        });

        // (mlme->sme) Send a DeauthenticateConf as a response
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: bss.bssid.to_array() },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: RSNA_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: RSNA_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
                rsn_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        "rsna_status": "PmkSaEstablished"
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        "rx_eapol_frame": AnyBytesProperty,
                        "status": "processed",
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "3": {
                        "@time": AnyNumericProperty,
                        "rx_eapol_frame": AnyBytesProperty,
                        "status": "processed",
                    },
                    "4": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                    "5": {
                        "@time": AnyNumericProperty,
                        "rx_eapol_frame": AnyBytesProperty,
                        "status": "processed",
                    },
                    "6": {
                        "@time": AnyNumericProperty,
                        "tx_eapol_frame": AnyBytesProperty,
                    },
                }
            },
        });
    }

    #[test]
    fn gtk_rotation_during_link_up() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = cmd.bss.bssid;
        let state = link_up_state(cmd);

        // (mlme->sme) Send an EapolInd, mock supplication with key frame and GTK
        let key_frame = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: true,
        };
        let gtk = SecAssocUpdate::Key(Key::Gtk(test_utils::gtk()));
        let mut state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![key_frame, gtk]);

        // EAPoL frame is sent out, but state still remains the same
        expect_eapol_req(&mut h.mlme_stream, bssid);
        expect_set_gtk(&mut h.mlme_stream);
        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");
        assert_variant!(&state, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::LinkUp { .. });
        });

        // Any timeout is ignored
        let (_, timed_event) = h.time_stream.try_next().unwrap().expect("expect timed event");
        state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
        assert_variant!(&state, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::LinkUp { .. });
        });

        // No new ConnectResult is sent
        assert_variant!(connect_txn_stream.try_next(), Err(_));
    }

    #[test]
    fn connect_while_link_up() {
        let mut h = TestHelper::new();
        let (cmd1, mut connect_txn_stream1) = connect_command_one();
        let (cmd2, mut connect_txn_stream2) = connect_command_two();
        let state = link_up_state(cmd1);
        let state = state.connect(cmd2, &mut h.context);
        let state = exchange_deauth(state, &mut h);

        // First stream should be dropped already
        assert_variant!(connect_txn_stream1.try_next(), Ok(None));
        // Second stream should either have event or is empty, but is not dropped
        assert_variant!(connect_txn_stream2.try_next(), Ok(Some(_)) | Err(_));

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(req))) => {
            assert_eq!(req.selected_bss, (*connect_command_two().0.bss).into());
        });
        assert_connecting(state, &connect_command_two().0.bss);
    }

    fn expect_state_events_link_up_roaming_link_up(
        inspector: &Inspector,
        selected_bss: BssDescription,
    ) {
        assert_data_tree!(inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        bssid: selected_bss.bssid.to_string(),
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: ROAMING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        bssid: selected_bss.bssid.to_string(),
                        ctx: AnyStringProperty,
                        from: ROAMING_STATE,
                        ssid: selected_bss.ssid.to_string(),
                        to: LINK_UP_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn fullmac_initiated_roam_happy_path_unprotected() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let mut selected_bss = cmd.bss.clone();
        let state = link_up_state(cmd);
        let selected_bssid = [0, 1, 2, 3, 4, 5];
        selected_bss.bssid = selected_bssid.into();
        #[allow(
            clippy::redundant_field_names,
            reason = "mass allow for https://fxbug.dev/381896734"
        )]
        let roam_start_ind = MlmeEvent::RoamStartInd {
            ind: fidl_mlme::RoamStartIndication {
                selected_bssid: selected_bssid,
                original_association_maintained: false,
                selected_bss: (*selected_bss).clone().into(),
            },
        };
        let state = state.on_mlme_event(roam_start_ind, &mut h.context);
        assert_roaming(&state);

        let mut association_ies = vec![];
        association_ies.extend_from_slice(selected_bss.ies());
        let ind = fidl_mlme::RoamResultIndication {
            selected_bssid,
            status_code: fidl_ieee80211::StatusCode::Success,
            original_association_maintained: false,
            target_bss_authenticated: true,
            association_id: 42,
            association_ies,
        };
        let roam_result_ind_event = MlmeEvent::RoamResultInd { ind: ind.clone() };
        let state = state.on_mlme_event(roam_result_ind_event, &mut h.context);
        assert_variant!(&state, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::LinkUp { .. });
        });

        // User should be notified that the roam succeeded.
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnRoamResult {result})) => {
            assert_eq!(result, RoamResult::Success(Box::new(*selected_bss.clone())));
        });

        expect_state_events_link_up_roaming_link_up(&h.inspector, *selected_bss.clone());
    }

    #[test]
    fn policy_initiated_roam_happy_path_unprotected() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let mut selected_bss = cmd.bss.clone();
        let state = link_up_state(cmd);
        let selected_bssid = [0, 1, 2, 3, 4, 5];
        selected_bss.bssid = selected_bssid.into();

        let fidl_selected_bss = fidl_common::BssDescription::from(*selected_bss.clone());
        let state = state.roam(&mut h.context, fidl_selected_bss);

        assert_roaming(&state);

        let mut association_ies = vec![];
        association_ies.extend_from_slice(selected_bss.ies());
        let conf = fidl_mlme::RoamConfirm {
            selected_bssid,
            status_code: fidl_ieee80211::StatusCode::Success,
            original_association_maintained: false,
            target_bss_authenticated: true,
            association_id: 42,
            association_ies,
        };
        let roam_conf_event = MlmeEvent::RoamConf { conf: conf.clone() };
        let state = state.on_mlme_event(roam_conf_event, &mut h.context);
        assert_variant!(&state, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::LinkUp { .. });
        });

        // User should be notified that the roam succeeded.
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnRoamResult {result})) => {
            assert_eq!(result, RoamResult::Success(Box::new(*selected_bss.clone())));
        });

        expect_state_events_link_up_roaming_link_up(&h.inspector, *selected_bss.clone());
    }

    fn expect_state_events_link_up_roaming_rsna(
        inspector: &Inspector,
        selected_bss: BssDescription,
    ) {
        assert_data_tree!(inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        bssid: selected_bss.bssid.to_string(),
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: ROAMING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        bssid: selected_bss.bssid.to_string(),
                        ctx: AnyStringProperty,
                        from: ROAMING_STATE,
                        ssid: selected_bss.ssid.to_string(),
                        to: RSNA_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn fullmac_initiated_roam_happy_path_protected() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();

        let (cmd, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = (*cmd.bss).clone();
        let mut selected_bss = bss.clone();
        let selected_bssid = [1, 2, 3, 4, 5, 6];
        selected_bss.bssid = selected_bssid.into();
        let association_ies = selected_bss.ies().to_vec();

        let state = link_up_state(cmd);
        // Initiate a roam attempt.
        #[allow(
            clippy::redundant_field_names,
            reason = "mass allow for https://fxbug.dev/381896734"
        )]
        let roam_start_ind = MlmeEvent::RoamStartInd {
            ind: fidl_mlme::RoamStartIndication {
                selected_bssid: selected_bssid,
                original_association_maintained: false,
                selected_bss: selected_bss.clone().into(),
            },
        };
        let state = state.on_mlme_event(roam_start_ind, &mut h.context);
        assert_roaming(&state);

        // Real supplicant would be reset here. Reset the mock supplicant.
        suppl_mock
            .set_start_updates(vec![SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)]);

        let ind = fidl_mlme::RoamResultIndication {
            selected_bssid,
            status_code: fidl_ieee80211::StatusCode::Success,
            original_association_maintained: false,
            target_bss_authenticated: true,
            association_id: 42,
            association_ies,
        };
        let roam_result_ind_event = MlmeEvent::RoamResultInd { ind: ind.clone() };
        let state = state.on_mlme_event(roam_result_ind_event, &mut h.context);

        assert_variant!(&state, ClientState::Associated(state)  => {
            assert_variant!(&state.link_state, LinkState::EstablishingRsna { .. });
        });

        // Note: because a new supplicant is created for the roam to the target, we can't easily
        // test the 802.1X portion of the roam.

        // User should be notified that the roam succeeded.
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnRoamResult {result})) => {
            assert_eq!(result, RoamResult::Success(Box::new(selected_bss.clone())));
        });

        expect_state_events_link_up_roaming_rsna(&h.inspector, selected_bss);
    }

    #[test]
    fn policy_initiated_roam_happy_path_protected() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();

        let (cmd, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = (*cmd.bss).clone();
        let mut selected_bss = bss.clone();
        let selected_bssid = [1, 2, 3, 4, 5, 6];
        selected_bss.bssid = selected_bssid.into();
        let association_ies = selected_bss.ies().to_vec();

        let state = link_up_state(cmd);
        // Initiate a roam attempt.
        let fidl_selected_bss = fidl_common::BssDescription::from(selected_bss.clone());
        let state = state.roam(&mut h.context, fidl_selected_bss);

        assert_roaming(&state);

        // Real supplicant would be reset here. Reset the mock supplicant.
        suppl_mock
            .set_start_updates(vec![SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)]);

        let conf = fidl_mlme::RoamConfirm {
            selected_bssid,
            status_code: fidl_ieee80211::StatusCode::Success,
            original_association_maintained: false,
            target_bss_authenticated: true,
            association_id: 42,
            association_ies,
        };
        let roam_conf_event = MlmeEvent::RoamConf { conf: conf.clone() };
        let state = state.on_mlme_event(roam_conf_event, &mut h.context);

        assert_variant!(&state, ClientState::Associated(state)  => {
            assert_variant!(&state.link_state, LinkState::EstablishingRsna { .. });
        });

        // Note: because a new supplicant is created for the roam to the target, we can't easily
        // test the 802.1X portion of the roam.

        // User should be notified that the roam succeeded.
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnRoamResult {result})) => {
            assert_eq!(result, RoamResult::Success(Box::new(selected_bss.clone())));
        });

        expect_state_events_link_up_roaming_rsna(&h.inspector, selected_bss);
    }

    fn expect_roam_failure_emitted(
        failure_type: RoamFailureType,
        status_code: fidl_ieee80211::StatusCode,
        selected_bssid: [u8; 6],
        mlme_event_name: fidl_sme::DisconnectMlmeEventName,
        connect_txn_stream: &mut ConnectTransactionStream,
    ) {
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnRoamResult { result })) => {
            assert_variant!(result, RoamResult::Failed(failure) => {
                assert_eq!(failure.failure_type, failure_type);
                assert_eq!(failure.status_code, status_code);
                assert_eq!(failure.selected_bssid, selected_bssid.into());
                assert_variant!(failure.disconnect_info.disconnect_source, fidl_sme::DisconnectSource::Mlme(cause) => {
                    assert_eq!(cause.mlme_event_name, mlme_event_name);
                });
            });
        });
    }

    // An all-zero BssDescription: malformed, in particular due to empty IEs (missing SSID).
    fn malformed_bss_description() -> fidl_common::BssDescription {
        fidl_common::BssDescription {
            bssid: [0, 0, 0, 0, 0, 0],
            bss_type: fidl_common::BssType::Infrastructure,
            beacon_period: 0,
            capability_info: 0,
            ies: Vec::new(),
            channel: fidl_common::WlanChannel {
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                primary: 0,
                secondary80: 0,
            },
            rssi_dbm: 0,
            snr_db: 0,
        }
    }

    fn expect_state_events_link_up_disconnecting_idle(inspector: &Inspector) {
        assert_data_tree!(inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn malformed_roam_start_ind_causes_disconnect() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = link_up_state(cmd);
        // Note: this is intentionally malformed. Roam cannot proceed without the missing data, such
        // as the IEs.
        let selected_bss = malformed_bss_description();
        // Note that this BSSID does not match the BssDescription.
        let selected_bssid = [0, 1, 2, 3, 4, 5];
        let roam_start_ind = MlmeEvent::RoamStartInd {
            ind: fidl_mlme::RoamStartIndication {
                selected_bssid,
                original_association_maintained: false,
                selected_bss,
            },
        };
        let state = state.on_mlme_event(roam_start_ind, &mut h.context);

        // Check that SME sends a deauthenticate request to target BSS, since roam started.
        expect_deauth_req(
            &mut h.mlme_stream,
            selected_bssid.into(),
            fidl_ieee80211::ReasonCode::StaLeaving,
        );

        // (mlme->sme) Send a DeauthenticateConf as a response, to advance to the post disconnect action.
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: selected_bssid },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        // Roam failure will have the target BSSID from the roam start ind.
        expect_roam_failure_emitted(
            RoamFailureType::RoamStartMalformedFailure,
            fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            selected_bssid,
            fidl_sme::DisconnectMlmeEventName::RoamStartIndication,
            &mut connect_txn_stream,
        );

        expect_state_events_link_up_disconnecting_idle(&h.inspector);
    }

    #[test]
    fn malformed_roam_req_causes_disconnect() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let original_bssid = cmd.bss.bssid;
        let state = link_up_state(cmd);
        // Note: this is intentionally malformed. Roam cannot proceed without the missing data, such
        // as the IEs.
        let selected_bss = malformed_bss_description();

        let state = state.roam(&mut h.context, selected_bss.clone());

        // Check that SME sends a deauthenticate request to the current BSS, since roam has not started.
        expect_deauth_req(
            &mut h.mlme_stream,
            original_bssid,
            fidl_ieee80211::ReasonCode::StaLeaving,
        );

        // (mlme->sme) Send a DeauthenticateConf as a response, to advance to the post disconnect action.
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: original_bssid.to_array() },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        // Malformed roam request will have the target BSSID from the roam request.
        expect_roam_failure_emitted(
            RoamFailureType::RoamRequestMalformedFailure,
            fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            selected_bss.bssid,
            fidl_sme::DisconnectMlmeEventName::RoamRequest,
            &mut connect_txn_stream,
        );

        expect_state_events_link_up_disconnecting_idle(&h.inspector);
    }

    #[test]
    fn roam_req_with_incorrect_security_ies_causes_disconnect_on_protected_network() {
        let mut h = TestHelper::new();
        let (supplicant, _suppl_mock) = mock_psk_supplicant();

        let (cmd, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let original_bssid = cmd.bss.bssid;

        // Note: intentionally incorrect security config is created for this test.
        let mut selected_bss = fake_bss_description!(Wpa1, ssid: Ssid::try_from("wpa2").unwrap());
        let selected_bssid = [3, 2, 1, 0, 9, 8];
        selected_bss.bssid = selected_bssid.into();

        let state = link_up_state(cmd);

        let state = state.roam(&mut h.context, selected_bss.into());

        // Check that SME sends a deauthenticate request to original BSS.
        expect_deauth_req(
            &mut h.mlme_stream,
            original_bssid,
            fidl_ieee80211::ReasonCode::StaLeaving,
        );

        // (mlme->sme) Send a DeauthenticateConf as a response, to advance to the post disconnect action.
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: original_bssid.to_array() },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        expect_roam_failure_emitted(
            RoamFailureType::SelectNetworkFailure,
            fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            selected_bssid,
            fidl_sme::DisconnectMlmeEventName::RoamRequest,
            &mut connect_txn_stream,
        );

        expect_state_events_link_up_disconnecting_idle(&h.inspector);
    }

    #[test]
    fn roam_start_ind_with_incorrect_security_ies_causes_disconnect_on_protected_network() {
        let mut h = TestHelper::new();
        let (supplicant, _suppl_mock) = mock_psk_supplicant();

        let (cmd, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        // Note: intentionally incorrect security config is created for this test.
        let mut selected_bss = fake_bss_description!(Wpa1, ssid: Ssid::try_from("wpa2").unwrap());
        let selected_bssid = [3, 2, 1, 0, 9, 8];
        selected_bss.bssid = selected_bssid.into();

        let state = link_up_state(cmd);
        let roam_start_ind = MlmeEvent::RoamStartInd {
            ind: fidl_mlme::RoamStartIndication {
                selected_bssid,
                original_association_maintained: false,
                selected_bss: selected_bss.into(),
            },
        };
        let state = state.on_mlme_event(roam_start_ind, &mut h.context);

        // Check that SME sends a deauthenticate request.
        expect_deauth_req(
            &mut h.mlme_stream,
            selected_bssid.into(),
            fidl_ieee80211::ReasonCode::StaLeaving,
        );

        // (mlme->sme) Send a DeauthenticateConf as a response, to advance to the post disconnect action.
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: selected_bssid },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        expect_roam_failure_emitted(
            RoamFailureType::SelectNetworkFailure,
            fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            selected_bssid,
            fidl_sme::DisconnectMlmeEventName::RoamStartIndication,
            &mut connect_txn_stream,
        );

        expect_state_events_link_up_disconnecting_idle(&h.inspector);
    }

    #[test]
    fn roam_start_ind_ignored_while_idle() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let mut selected_bss = cmd.bss.clone();
        let state = idle_state();
        let selected_bssid = [0, 1, 2, 3, 4, 5];
        selected_bss.bssid = selected_bssid.into();
        let roam_start_ind = MlmeEvent::RoamStartInd {
            ind: fidl_mlme::RoamStartIndication {
                selected_bssid,
                original_association_maintained: false,
                selected_bss: (*selected_bss).into(),
            },
        };
        let state = state.on_mlme_event(roam_start_ind, &mut h.context);
        assert_idle(state);

        assert_variant!(connect_txn_stream.try_next(), Err(_));
    }

    #[test]
    fn roam_req_ignored_while_idle() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let mut selected_bss = cmd.bss.clone();
        let state = idle_state();
        let selected_bssid = [0, 1, 2, 3, 4, 5];
        selected_bss.bssid = selected_bssid.into();
        let fidl_selected_bss = fidl_common::BssDescription::from(*selected_bss.clone());

        let state = state.roam(&mut h.context, fidl_selected_bss);

        assert_idle(state);

        assert_variant!(connect_txn_stream.try_next(), Err(_));
    }

    #[test]
    fn roam_start_ind_ignored_while_connecting() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let original_bss = cmd.bss.clone();
        let mut selected_bss = cmd.bss.clone();
        let state = connecting_state(cmd);
        let selected_bssid = [0, 1, 2, 3, 4, 5];
        selected_bss.bssid = selected_bssid.into();
        let roam_start_ind = MlmeEvent::RoamStartInd {
            ind: fidl_mlme::RoamStartIndication {
                selected_bssid,
                original_association_maintained: false,
                selected_bss: (*selected_bss).into(),
            },
        };
        let state = state.on_mlme_event(roam_start_ind, &mut h.context);
        assert_connecting(state, &original_bss);

        // Nothing should be sent upward.
        assert_variant!(connect_txn_stream.try_next(), Ok(None));
    }

    #[test]
    fn roam_req_ignored_while_connecting() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let original_bss = cmd.bss.clone();
        let mut selected_bss = cmd.bss.clone();
        let state = connecting_state(cmd);
        let selected_bssid = [0, 1, 2, 3, 4, 5];
        selected_bss.bssid = selected_bssid.into();
        let fidl_selected_bss = fidl_common::BssDescription::from(*selected_bss.clone());

        let state = state.roam(&mut h.context, fidl_selected_bss);

        assert_connecting(state, &original_bss);

        // Nothing should be sent upward.
        assert_variant!(connect_txn_stream.try_next(), Ok(None));
    }

    #[test]
    fn roam_start_ind_ignored_while_disconnecting() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let mut selected_bss = cmd.bss.clone();
        let state = disconnecting_state(PostDisconnectAction::BeginConnect { cmd });
        let selected_bssid = [0, 1, 2, 3, 4, 5];
        selected_bss.bssid = selected_bssid.into();
        let roam_start_ind = MlmeEvent::RoamStartInd {
            ind: fidl_mlme::RoamStartIndication {
                selected_bssid,
                original_association_maintained: false,
                selected_bss: (*selected_bss).into(),
            },
        };
        let state = state.on_mlme_event(roam_start_ind, &mut h.context);
        assert_disconnecting(state);

        // Nothing should be sent upward.
        assert_variant!(connect_txn_stream.try_next(), Ok(None));
    }

    #[test]
    fn roam_req_ignored_while_disconnecting() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let mut selected_bss = cmd.bss.clone();
        let state = disconnecting_state(PostDisconnectAction::BeginConnect { cmd });
        let selected_bssid = [0, 1, 2, 3, 4, 5];
        selected_bss.bssid = selected_bssid.into();
        let fidl_selected_bss = fidl_common::BssDescription::from(*selected_bss.clone());

        let state = state.roam(&mut h.context, fidl_selected_bss);

        assert_disconnecting(state);

        // Nothing should be sent upward.
        assert_variant!(connect_txn_stream.try_next(), Ok(None));
    }

    fn expect_state_events_roaming_disconnecting_idle(inspector: &Inspector) {
        assert_data_tree!(inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: ROAMING_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn roam_result_ind_with_failure_causes_disconnect() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let selected_bssid = [1, 2, 3, 4, 5, 6];
        let state = roaming_state(cmd, selected_bssid.into());
        let status_code = fidl_ieee80211::StatusCode::RefusedUnauthenticatedAccessNotSupported;
        #[allow(
            clippy::redundant_field_names,
            reason = "mass allow for https://fxbug.dev/381896734"
        )]
        let roam_result_ind = MlmeEvent::RoamResultInd {
            ind: fidl_mlme::RoamResultIndication {
                selected_bssid: selected_bssid,
                status_code,
                original_association_maintained: false,
                target_bss_authenticated: true,
                association_id: 0,
                association_ies: Vec::new(),
            },
        };
        let state = state.on_mlme_event(roam_result_ind, &mut h.context);

        // Check that SME sends a deauthenticate request.
        expect_deauth_req(
            &mut h.mlme_stream,
            selected_bssid.into(),
            fidl_ieee80211::ReasonCode::StaLeaving,
        );

        // (mlme->sme) Send a DeauthenticateConf as a response, to advance to the post disconnect action.
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: selected_bssid },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        expect_roam_failure_emitted(
            RoamFailureType::ReassociationFailure,
            status_code,
            selected_bssid,
            fidl_sme::DisconnectMlmeEventName::RoamResultIndication,
            &mut connect_txn_stream,
        );

        expect_state_events_roaming_disconnecting_idle(&h.inspector);
    }

    #[test]
    fn malformed_roam_result_ind_causes_disconnect() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let selected_bssid = [1, 2, 3, 4, 5, 6];
        let mismatched_bssid = [9, 8, 7, 6, 5, 4];
        let state = roaming_state(cmd, selected_bssid.into());
        let status_code = fidl_ieee80211::StatusCode::Success;
        let roam_result_ind = MlmeEvent::RoamResultInd {
            ind: fidl_mlme::RoamResultIndication {
                selected_bssid: mismatched_bssid,
                status_code,
                original_association_maintained: false,
                target_bss_authenticated: true,
                association_id: 0,
                association_ies: Vec::new(),
            },
        };
        let state = state.on_mlme_event(roam_result_ind, &mut h.context);

        // Check that SME sends a deauthenticate request.
        expect_deauth_req(
            &mut h.mlme_stream,
            selected_bssid.into(),
            fidl_ieee80211::ReasonCode::StaLeaving,
        );

        // (mlme->sme) Send a DeauthenticateConf as a response, to advance to the post disconnect action.
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: selected_bssid },
        };
        let state = state.on_mlme_event(deauth_conf, &mut h.context);
        assert_idle(state);

        expect_roam_failure_emitted(
            RoamFailureType::RoamResultMalformedFailure,
            fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            selected_bssid,
            fidl_sme::DisconnectMlmeEventName::RoamResultIndication,
            &mut connect_txn_stream,
        );

        expect_state_events_roaming_disconnecting_idle(&h.inspector);
    }

    fn make_disconnect_request(
        h: &mut TestHelper,
    ) -> (
        <fidl_sme::ClientSmeProxy as fidl_sme::ClientSmeProxyInterface>::DisconnectResponseFut,
        fidl_sme::ClientSmeDisconnectResponder,
    ) {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fidl_sme::ClientSmeMarker>();
        let mut disconnect_fut =
            proxy.disconnect(fidl_sme::UserDisconnectReason::DisconnectDetectedFromSme);
        assert_variant!(h.executor.run_until_stalled(&mut disconnect_fut), Poll::Pending);
        let responder = assert_variant!(
            h.executor.run_singlethreaded(stream.next()).unwrap().unwrap(),
            fidl_sme::ClientSmeRequest::Disconnect{ responder, .. } => responder);
        (disconnect_fut, responder)
    }

    #[test]
    fn disconnect_while_idle() {
        let mut h = TestHelper::new();
        let (mut disconnect_fut, responder) = make_disconnect_request(&mut h);
        let new_state = idle_state().disconnect(
            &mut h.context,
            fidl_sme::UserDisconnectReason::WlanSmeUnitTesting,
            responder,
        );
        assert_idle(new_state);
        // Expect no messages to the MLME
        assert!(h.mlme_stream.try_next().is_err());
        assert_variant!(h.executor.run_until_stalled(&mut disconnect_fut), Poll::Ready(Ok(())));

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: IDLE_STATE,
                        to: IDLE_STATE,
                    },
                }
            },
        });
    }

    #[test]
    fn disconnect_while_connecting() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = connecting_state(cmd);
        let state = disconnect(state, &mut h, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        let state = exchange_deauth(state, &mut h);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Canceled);
        });
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn disconnect_while_link_up() {
        let mut h = TestHelper::new();
        let state = link_up_state(connect_command_one().0);
        let state = disconnect(state, &mut h, fidl_sme::UserDisconnectReason::FailedToConnect);
        let state = exchange_deauth(state, &mut h);
        assert_idle(state);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn timeout_during_disconnect() {
        let mut h = TestHelper::new();
        let (cmd, _connect_txn_stream) = connect_command_one();
        let mut state = link_up_state(cmd);

        let (mut disconnect_fut, responder) = make_disconnect_request(&mut h);
        state = state.disconnect(
            &mut h.context,
            fidl_sme::UserDisconnectReason::DisconnectDetectedFromSme,
            responder,
        );
        assert_variant!(&state, ClientState::Disconnecting(_));

        let timed_event =
            assert_variant!(h.time_stream.try_next(), Ok(Some((_, timed_event))) => timed_event);
        assert_variant!(timed_event.event, Event::DeauthenticateTimeout(..));

        let state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
        assert_idle(state);
        assert_variant!(h.executor.run_until_stalled(&mut disconnect_fut), Poll::Ready(Ok(())));

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn new_connect_while_disconnecting() {
        let mut h = TestHelper::new();
        let (cmd1, _connect_txn_stream) = connect_command_one();
        let (cmd2, _connect_txn_stream) = connect_command_two();
        let bss2 = cmd2.bss.clone();
        let state = link_up_state(cmd1);
        let (mut disconnect_fut, responder) = make_disconnect_request(&mut h);
        let state = state.disconnect(
            &mut h.context,
            fidl_sme::UserDisconnectReason::DisconnectDetectedFromSme,
            responder,
        );

        let disconnecting =
            assert_variant!(&state, ClientState::Disconnecting(disconnecting) => disconnecting);
        assert_variant!(&disconnecting.action, PostDisconnectAction::RespondDisconnect { .. });
        assert_variant!(h.executor.run_until_stalled(&mut disconnect_fut), Poll::Pending);

        let state = state.connect(cmd2, &mut h.context);
        let disconnecting =
            assert_variant!(&state, ClientState::Disconnecting(disconnecting) => disconnecting);
        assert_variant!(&disconnecting.action, PostDisconnectAction::BeginConnect { .. });
        assert_variant!(h.executor.run_until_stalled(&mut disconnect_fut), Poll::Ready(Ok(())));
        let state = exchange_deauth(state, &mut h);
        assert_connecting(state, &connect_command_two().0.bss);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: DISCONNECTING_STATE,
                        to: CONNECTING_STATE,
                        bssid: bss2.bssid.to_string(),
                        ssid: bss2.ssid.to_string(),
                    },
                },
            },
        });
    }

    #[test]
    fn disconnect_while_disconnecting_for_pending_connect() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = disconnecting_state(PostDisconnectAction::BeginConnect { cmd });

        let (_fut, responder) = make_disconnect_request(&mut h);
        let state = state.disconnect(
            &mut h.context,
            fidl_sme::UserDisconnectReason::DisconnectDetectedFromSme,
            responder,
        );
        let disconnecting =
            assert_variant!(&state, ClientState::Disconnecting(disconnecting) => disconnecting);
        assert_variant!(&disconnecting.action, PostDisconnectAction::RespondDisconnect { .. });

        let result = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnConnectResult { result, .. })) => result
        );
        assert_eq!(result, ConnectResult::Canceled);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: DISCONNECTING_STATE,
                        to: DISCONNECTING_STATE,
                    }
                },
            },
        });
    }

    #[test]
    fn increment_att_id_on_connect() {
        let mut h = TestHelper::new();
        let state = idle_state();
        assert_eq!(h.context.att_id, 0);

        let state = state.connect(connect_command_one().0, &mut h.context);
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(_))));
        assert_eq!(h.context.att_id, 1);

        let state = disconnect(state, &mut h, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        let state = exchange_deauth(state, &mut h);
        assert_eq!(h.context.att_id, 1);

        let state = state.connect(connect_command_two().0, &mut h.context);
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(_))));
        assert_eq!(h.context.att_id, 2);

        let state = state.connect(connect_command_one().0, &mut h.context);
        let _state = exchange_deauth(state, &mut h);
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(_))));
        assert_eq!(h.context.att_id, 3);
    }

    #[test]
    fn increment_att_id_on_disassociate_ind() {
        let mut h = TestHelper::new();
        let (cmd, _connect_txn_stream) = connect_command_one();
        let bss = cmd.bss.clone();
        let state = link_up_state(cmd);
        assert_eq!(h.context.att_id, 0);

        let disassociate_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::UnspecifiedReason,
                locally_initiated: false,
            },
        };

        let state = state.on_mlme_event(disassociate_ind, &mut h.context);
        assert_variant!(&state, ClientState::Connecting(connecting) =>
                            assert_eq!(connecting.reassociation_loop_count, 1));
        assert_connecting(state, &bss);
        assert_eq!(h.context.att_id, 1);
    }

    #[test]
    fn abort_connect_after_max_associate_retries() {
        let mut h = TestHelper::new();
        let (supplicant, _suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let mut state = establishing_rsna_state(command);

        match &mut state {
            ClientState::Associated(associated) => {
                associated.reassociation_loop_count = MAX_REASSOCIATIONS_WITHOUT_LINK_UP;
            }
            _ => unreachable!(),
        }

        let disassociate_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::UnacceptablePowerCapability,
                locally_initiated: true,
            },
        };
        let state = state.on_mlme_event(disassociate_ind, &mut h.context);

        // We should notify of a disconnect and stop any retries.
        assert_disconnecting(state);
        let info = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { info })) => info
        );
        assert!(!info.is_sme_reconnecting);

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: RSNA_STATE,
                        to: DISCONNECTING_STATE,
                    }
                },
            },
        });
    }

    #[test]
    fn do_not_log_disconnect_ctx_on_disassoc_from_non_link_up() {
        let mut h = TestHelper::new();
        let (supplicant, _suppl_mock) = mock_psk_supplicant();
        let (command, _connect_txn_stream) = connect_command_wpa2(supplicant);
        let state = establishing_rsna_state(command);

        let disassociate_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::UnacceptablePowerCapability,
                locally_initiated: true,
            },
        };
        let state = state.on_mlme_event(disassociate_ind, &mut h.context);
        assert_connecting(
            state,
            &fake_bss_description!(Wpa2, ssid: Ssid::try_from("wpa2").unwrap()),
        );

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: RSNA_STATE,
                        to: CONNECTING_STATE,
                    }
                },
            },
        });
    }

    #[test]
    fn disconnect_reported_on_deauth_ind() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = link_up_state(cmd);

        let deauth_ind = MlmeEvent::DeauthenticateInd {
            ind: fidl_mlme::DeauthenticateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                locally_initiated: true,
            },
        };

        let _state = state.on_mlme_event(deauth_ind, &mut h.context);
        let fidl_info = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { info })) => info
        );
        assert!(!fidl_info.is_sme_reconnecting);
        assert_eq!(
            fidl_info.disconnect_source,
            fidl_sme::DisconnectSource::Mlme(fidl_sme::DisconnectCause {
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
            })
        );

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: IDLE_STATE,
                    }
                },
            },
        });
    }

    #[test]
    fn disconnect_reported_on_disassoc_ind_then_reconnect_successfully() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let bss = cmd.bss.clone();
        let state = link_up_state(cmd);

        let deauth_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::ReasonInactivity,
                locally_initiated: true,
            },
        };

        let state = state.on_mlme_event(deauth_ind, &mut h.context);
        let info = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { info })) => info
        );
        assert!(info.is_sme_reconnecting);
        assert_eq!(
            info.disconnect_source,
            fidl_sme::DisconnectSource::Mlme(fidl_sme::DisconnectCause {
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DisassociateIndication,
                reason_code: fidl_ieee80211::ReasonCode::ReasonInactivity,
            })
        );

        // Check that reconnect is attempted
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Reconnect(req))) => {
            assert_eq!(&req.peer_sta_address, bss.bssid.as_array());
        });

        // (mlme->sme) Send a ConnectConf
        let connect_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let _state = state.on_mlme_event(connect_conf, &mut h.context);

        // User should be notified that we are reconnected
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect })) => {
            assert_eq!(result, ConnectResult::Success);
            assert!(is_reconnect);
        });

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: CONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: LINK_UP_STATE,
                    }
                },
            },
        });
    }

    #[test]
    fn disconnect_reported_on_disassoc_ind_then_reconnect_unsuccessfully() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let bss = cmd.bss.clone();
        let state = link_up_state(cmd);

        let disassoc_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::ReasonInactivity,
                locally_initiated: true,
            },
        };

        let state = state.on_mlme_event(disassoc_ind, &mut h.context);
        assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { .. }))
        );

        // Check that reconnect is attempted
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Reconnect(req))) => {
            assert_eq!(&req.peer_sta_address, bss.bssid.as_array());
        });

        // (mlme->sme) Send a ConnectConf
        let connect_conf =
            create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::RefusedReasonUnspecified);
        let state = state.on_mlme_event(connect_conf, &mut h.context);

        let state = exchange_deauth(state, &mut h);
        assert_idle(state);

        // User should be notified that reconnection attempt failed
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect })) => {
            assert_eq!(result, AssociationFailure {
                bss_protection: BssProtection::Open,
                code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            }.into());
            assert!(is_reconnect);
        });

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: CONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: CONNECTING_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "2": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    },
                },
            },
        });
    }

    #[test]
    fn disconnect_reported_on_manual_disconnect() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = link_up_state(cmd);

        let state = disconnect(state, &mut h, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        assert_idle(state);
        let info = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { info })) => info
        );
        assert!(!info.is_sme_reconnecting);
        assert_eq!(
            info.disconnect_source,
            fidl_sme::DisconnectSource::User(fidl_sme::UserDisconnectReason::WlanSmeUnitTesting)
        );

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    }
                },
            },
        });
    }

    #[test]
    fn disconnect_reported_on_manual_disconnect_with_wsc() {
        let mut h = TestHelper::new();
        let (mut cmd, mut connect_txn_stream) = connect_command_one();
        cmd.bss = Box::new(fake_bss_description!(Open,
            ssid: Ssid::try_from("bar").unwrap(),
            bssid: [8; 6],
            rssi_dbm: 60,
            snr_db: 30,
            ies_overrides: IesOverrides::new().set_raw(
                get_vendor_ie_bytes_for_wsc_ie(&fake_probe_resp_wsc_ie_bytes()).expect("getting vendor ie bytes")
        )));

        let state = link_up_state(cmd);
        let state = disconnect(state, &mut h, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        assert_idle(state);

        let info = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { info })) => info
        );
        assert!(!info.is_sme_reconnecting);
        assert_eq!(
            info.disconnect_source,
            fidl_sme::DisconnectSource::User(fidl_sme::UserDisconnectReason::WlanSmeUnitTesting)
        );

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: LINK_UP_STATE,
                        to: DISCONNECTING_STATE,
                    },
                    "1": {
                        "@time": AnyNumericProperty,
                        from: DISCONNECTING_STATE,
                        to: IDLE_STATE,
                    }
                },
            },
        });
    }

    #[test]
    fn bss_channel_switch_ind() {
        let mut h = TestHelper::new();
        let (mut cmd, mut connect_txn_stream) = connect_command_one();
        cmd.bss = Box::new(fake_bss_description!(Open,
            ssid: Ssid::try_from("bar").unwrap(),
            bssid: [8; 6],
            channel: Channel::new(1, Cbw::Cbw20),
        ));
        let state = link_up_state(cmd);

        let input_info = fidl_internal::ChannelSwitchInfo { new_channel: 36 };
        let switch_ind = MlmeEvent::OnChannelSwitched { info: input_info };

        assert_variant!(&state, ClientState::Associated(state) => {
            assert_eq!(state.latest_ap_state.channel.primary, 1);
        });
        let state = state.on_mlme_event(switch_ind, &mut h.context);
        assert_variant!(state, ClientState::Associated(state) => {
            assert_eq!(state.latest_ap_state.channel.primary, 36);
        });

        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnChannelSwitched { info })) => {
            assert_eq!(info, input_info);
        });
    }

    #[test]
    fn connect_failure_rsne_wrapped_in_legacy_wpa() {
        let (supplicant, _suppl_mock) = mock_psk_supplicant();

        let (mut command, _connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = command.bss.clone();
        // Take the RSNA and wrap it in LegacyWpa to make it invalid.
        if let Protection::Rsna(rsna) = command.protection {
            command.protection = Protection::LegacyWpa(rsna);
        } else {
            panic!("command is guaranteed to be contain legacy wpa");
        };

        let mut h = TestHelper::new();
        let state = idle_state().connect(command, &mut h.context);

        // State did not change to Connecting because command is invalid, thus ignored.
        assert_variant!(state, ClientState::Idle(_));

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: IDLE_STATE,
                        to: IDLE_STATE,
                        bssid: bss.bssid.to_string(),
                        ssid: bss.ssid.to_string(),
                    }
                },
            },
        });
    }

    #[test]
    fn connect_failure_legacy_wpa_wrapped_in_rsna() {
        let (supplicant, _suppl_mock) = mock_psk_supplicant();

        let (mut command, _connect_txn_stream) = connect_command_wpa1(supplicant);
        let bss = command.bss.clone();
        // Take the LegacyWpa RSNA and wrap it in Rsna to make it invalid.
        if let Protection::LegacyWpa(rsna) = command.protection {
            command.protection = Protection::Rsna(rsna);
        } else {
            panic!("command is guaranteed to be contain legacy wpa");
        };

        let mut h = TestHelper::new();
        let state = idle_state();
        let state = state.connect(command, &mut h.context);

        // State did not change to Connecting because command is invalid, thus ignored.
        assert_variant!(state, ClientState::Idle(_));

        assert_data_tree!(h.inspector, root: contains {
            usme: contains {
                state_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        ctx: AnyStringProperty,
                        from: IDLE_STATE,
                        to: IDLE_STATE,
                        bssid: bss.bssid.to_string(),
                        ssid: bss.ssid.to_string(),
                    }
                },
            },
        });
    }

    #[test]
    fn status_returns_last_rssi_snr() {
        let mut h = TestHelper::new();
        let time_a = now();

        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = link_up_state(cmd);
        let input_ind = fidl_internal::SignalReportIndication { rssi_dbm: -42, snr_db: 20 };
        let state = state.on_mlme_event(MlmeEvent::SignalReport { ind: input_ind }, &mut h.context);
        let serving_ap_info = assert_variant!(state.status(),
                                                     ClientSmeStatus::Connected(serving_ap_info) =>
                                                     serving_ap_info);
        assert_eq!(serving_ap_info.rssi_dbm, -42);
        assert_eq!(serving_ap_info.snr_db, 20);
        assert!(serving_ap_info.signal_report_time > time_a);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnSignalReport { ind })) => {
            assert_eq!(input_ind, ind);
        });

        let time_b = now();
        let signal_report_time = assert_variant!(state.status(),
                                                 ClientSmeStatus::Connected(serving_ap_info) =>
                                                 serving_ap_info.signal_report_time);
        assert!(signal_report_time < time_b);

        let input_ind = fidl_internal::SignalReportIndication { rssi_dbm: -24, snr_db: 10 };
        let state = state.on_mlme_event(MlmeEvent::SignalReport { ind: input_ind }, &mut h.context);
        let serving_ap_info = assert_variant!(state.status(),
                                                     ClientSmeStatus::Connected(serving_ap_info) =>
                                                     serving_ap_info);
        assert_eq!(serving_ap_info.rssi_dbm, -24);
        assert_eq!(serving_ap_info.snr_db, 10);
        let signal_report_time = assert_variant!(state.status(),
                                                 ClientSmeStatus::Connected(serving_ap_info) =>
                                                 serving_ap_info.signal_report_time);
        assert!(signal_report_time > time_b);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnSignalReport { ind })) => {
            assert_eq!(input_ind, ind);
        });

        let time_c = now();
        let signal_report_time = assert_variant!(state.status(),
                                                 ClientSmeStatus::Connected(serving_ap_info) =>
                                                 serving_ap_info.signal_report_time);
        assert!(signal_report_time < time_c);
    }

    fn test_sae_frame_rx_tx(
        mock_supplicant_controller: MockSupplicantController,
        state: ClientState,
    ) -> ClientState {
        let mut h = TestHelper::new();
        let frame_rx = fidl_mlme::SaeFrame {
            peer_sta_address: [0xaa; 6],
            status_code: fidl_ieee80211::StatusCode::Success,
            seq_num: 1,
            sae_fields: vec![1, 2, 3, 4, 5],
        };
        let frame_tx = fidl_mlme::SaeFrame {
            peer_sta_address: [0xbb; 6],
            status_code: fidl_ieee80211::StatusCode::Success,
            seq_num: 2,
            sae_fields: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        mock_supplicant_controller
            .set_on_sae_frame_rx_updates(vec![SecAssocUpdate::TxSaeFrame(frame_tx)]);
        let state =
            state.on_mlme_event(MlmeEvent::OnSaeFrameRx { frame: frame_rx }, &mut h.context);
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::SaeFrameTx(_))));
        state
    }

    #[test]
    fn sae_sends_frame_in_connecting() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = connecting_state(cmd);
        let end_state = test_sae_frame_rx_tx(suppl_mock, state);
        assert_variant!(end_state, ClientState::Connecting(_))
    }

    fn test_sae_frame_ind_resp(
        mock_supplicant_controller: MockSupplicantController,
        state: ClientState,
    ) -> ClientState {
        let mut h = TestHelper::new();
        let ind = fidl_mlme::SaeHandshakeIndication { peer_sta_address: [0xaa; 6] };
        // For the purposes of the test, skip the rx/tx and just say we succeeded.
        mock_supplicant_controller.set_on_sae_handshake_ind_updates(vec![
            SecAssocUpdate::SaeAuthStatus(AuthStatus::Success),
        ]);
        let state = state.on_mlme_event(MlmeEvent::OnSaeHandshakeInd { ind }, &mut h.context);

        let resp = assert_variant!(
            h.mlme_stream.try_next(),
            Ok(Some(MlmeRequest::SaeHandshakeResp(resp))) => resp);
        assert_eq!(resp.status_code, fidl_ieee80211::StatusCode::Success);
        state
    }

    #[test]
    fn sae_ind_in_connecting() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = connecting_state(cmd);
        let end_state = test_sae_frame_ind_resp(suppl_mock, state);
        assert_variant!(end_state, ClientState::Connecting(_))
    }

    fn test_sae_timeout(
        mock_supplicant_controller: MockSupplicantController,
        state: ClientState,
    ) -> ClientState {
        let mut h = TestHelper::new();
        let frame_tx = fidl_mlme::SaeFrame {
            peer_sta_address: [0xbb; 6],
            status_code: fidl_ieee80211::StatusCode::Success,
            seq_num: 2,
            sae_fields: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        mock_supplicant_controller
            .set_on_sae_timeout_updates(vec![SecAssocUpdate::TxSaeFrame(frame_tx)]);
        let state = state.handle_timeout(1, event::SaeTimeout(2).into(), &mut h.context);
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::SaeFrameTx(_))));
        state
    }

    fn test_sae_timeout_failure(
        mock_supplicant_controller: MockSupplicantController,
        state: ClientState,
    ) {
        let mut h = TestHelper::new();
        mock_supplicant_controller
            .set_on_sae_timeout_failure(anyhow::anyhow!("Failed to process timeout"));
        let state = state.handle_timeout(1, event::SaeTimeout(2).into(), &mut h.context);
        let state = exchange_deauth(state, &mut h);
        assert_variant!(state, ClientState::Idle(_))
    }

    #[test]
    fn sae_timeout_in_connecting() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = connecting_state(cmd);
        let end_state = test_sae_timeout(suppl_mock, state);
        assert_variant!(end_state, ClientState::Connecting(_));
    }

    #[test]
    fn sae_timeout_failure_in_connecting() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = connecting_state(cmd);
        test_sae_timeout_failure(suppl_mock, state);
    }

    #[test]
    fn update_wmm_ac_params_new() {
        let mut h = TestHelper::new();
        let wmm_param = None;
        let state = link_up_state_with_wmm(connect_command_one().0, wmm_param);

        let state = state.on_mlme_event(create_on_wmm_status_resp(zx::sys::ZX_OK), &mut h.context);
        assert_variant!(state, ClientState::Associated(state) => {
            assert_variant!(state.wmm_param, Some(wmm_param) => {
                assert!(wmm_param.wmm_info.ap_wmm_info().uapsd());
                assert_wmm_param_acs(&wmm_param);
            })
        });
    }

    #[test]
    fn update_wmm_ac_params_existing() {
        let mut h = TestHelper::new();

        let existing_wmm_param =
            *ie::parse_wmm_param(&fake_wmm_param().bytes[..]).expect("parse wmm");
        existing_wmm_param.wmm_info.ap_wmm_info().set_uapsd(false);
        let state = link_up_state_with_wmm(connect_command_one().0, Some(existing_wmm_param));

        let state = state.on_mlme_event(create_on_wmm_status_resp(zx::sys::ZX_OK), &mut h.context);
        assert_variant!(state, ClientState::Associated(state) => {
            assert_variant!(state.wmm_param, Some(wmm_param) => {
                assert!(wmm_param.wmm_info.ap_wmm_info().uapsd());
                assert_wmm_param_acs(&wmm_param);
            })
        });
    }

    #[test]
    fn update_wmm_ac_params_fails() {
        let mut h = TestHelper::new();

        let existing_wmm_param =
            *ie::parse_wmm_param(&fake_wmm_param().bytes[..]).expect("parse wmm");
        let state = link_up_state_with_wmm(connect_command_one().0, Some(existing_wmm_param));

        let state = state
            .on_mlme_event(create_on_wmm_status_resp(zx::sys::ZX_ERR_UNAVAILABLE), &mut h.context);
        assert_variant!(state, ClientState::Associated(state) => {
            assert_variant!(state.wmm_param, Some(wmm_param) => {
                assert_eq!(wmm_param, existing_wmm_param);
            })
        });
    }

    fn assert_wmm_param_acs(wmm_param: &ie::WmmParam) {
        assert_eq!(wmm_param.ac_be_params.aci_aifsn.aifsn(), 1);
        assert!(!wmm_param.ac_be_params.aci_aifsn.acm());
        assert_eq!(wmm_param.ac_be_params.ecw_min_max.ecw_min(), 2);
        assert_eq!(wmm_param.ac_be_params.ecw_min_max.ecw_max(), 3);
        assert_eq!({ wmm_param.ac_be_params.txop_limit }, 4);

        assert_eq!(wmm_param.ac_bk_params.aci_aifsn.aifsn(), 5);
        assert!(!wmm_param.ac_bk_params.aci_aifsn.acm());
        assert_eq!(wmm_param.ac_bk_params.ecw_min_max.ecw_min(), 6);
        assert_eq!(wmm_param.ac_bk_params.ecw_min_max.ecw_max(), 7);
        assert_eq!({ wmm_param.ac_bk_params.txop_limit }, 8);

        assert_eq!(wmm_param.ac_vi_params.aci_aifsn.aifsn(), 9);
        assert!(wmm_param.ac_vi_params.aci_aifsn.acm());
        assert_eq!(wmm_param.ac_vi_params.ecw_min_max.ecw_min(), 10);
        assert_eq!(wmm_param.ac_vi_params.ecw_min_max.ecw_max(), 11);
        assert_eq!({ wmm_param.ac_vi_params.txop_limit }, 12);

        assert_eq!(wmm_param.ac_vo_params.aci_aifsn.aifsn(), 13);
        assert!(wmm_param.ac_vo_params.aci_aifsn.acm());
        assert_eq!(wmm_param.ac_vo_params.ecw_min_max.ecw_min(), 14);
        assert_eq!(wmm_param.ac_vo_params.ecw_min_max.ecw_max(), 15);
        assert_eq!({ wmm_param.ac_vo_params.txop_limit }, 16);
    }

    // Helper functions and data structures for tests
    struct TestHelper {
        mlme_stream: MlmeStream,
        time_stream: timer::EventStream<Event>,
        context: Context,
        // Inspector is kept so that root node doesn't automatically get removed from VMO
        inspector: Inspector,
        // Executor is needed as a time provider for the [`inspect_log!`] macro which panics
        // without a fuchsia_async executor set up
        executor: fuchsia_async::TestExecutor,
    }

    impl TestHelper {
        fn new_(with_fake_time: bool) -> Self {
            let executor = if with_fake_time {
                fuchsia_async::TestExecutor::new_with_fake_time()
            } else {
                fuchsia_async::TestExecutor::new()
            };

            let (mlme_sink, mlme_stream) = mpsc::unbounded();
            let (timer, time_stream) = timer::create_timer();
            let inspector = Inspector::default();
            let context = Context {
                device_info: Arc::new(fake_device_info()),
                mlme_sink: MlmeSink::new(mlme_sink),
                timer,
                att_id: 0,
                inspect: Arc::new(inspect::SmeTree::new(
                    inspector.clone(),
                    inspector.root().create_child("usme"),
                    &test_utils::fake_device_info([1u8; 6].into()),
                    &fake_spectrum_management_support_empty(),
                )),
                mac_sublayer_support: fake_mac_sublayer_support(),
                security_support: fake_security_support(),
            };
            TestHelper { mlme_stream, time_stream, context, inspector, executor }
        }
        fn new() -> Self {
            Self::new_(false)
        }
        fn new_with_fake_time() -> Self {
            Self::new_(true)
        }
    }

    fn on_eapol_ind(
        state: ClientState,
        helper: &mut TestHelper,
        bssid: Bssid,
        suppl_mock: &MockSupplicantController,
        update_sink: UpdateSink,
    ) -> ClientState {
        suppl_mock.set_on_eapol_frame_updates(update_sink);
        // (mlme->sme) Send an EapolInd
        let eapol_ind = create_eapol_ind(bssid, test_utils::eapol_key_frame().into());
        state.on_mlme_event(eapol_ind, &mut helper.context)
    }

    fn on_set_keys_conf(
        state: ClientState,
        helper: &mut TestHelper,
        key_ids: Vec<u16>,
    ) -> ClientState {
        state.on_mlme_event(
            MlmeEvent::SetKeysConf {
                conf: fidl_mlme::SetKeysConfirm {
                    results: key_ids
                        .into_iter()
                        .map(|key_id| fidl_mlme::SetKeyResult {
                            key_id,
                            status: zx::Status::OK.into_raw(),
                        })
                        .collect(),
                },
            },
            &mut helper.context,
        )
    }

    fn create_eapol_ind(bssid: Bssid, data: Vec<u8>) -> MlmeEvent {
        MlmeEvent::EapolInd {
            ind: fidl_mlme::EapolIndication {
                src_addr: bssid.to_array(),
                dst_addr: fake_device_info().sta_addr,
                data,
            },
        }
    }

    fn exchange_deauth(state: ClientState, h: &mut TestHelper) -> ClientState {
        // (sme->mlme) Expect a DeauthenticateRequest
        let peer_sta_address = assert_variant!(
            h.mlme_stream.try_next(),
            Ok(Some(MlmeRequest::Deauthenticate(req))) => req.peer_sta_address
        );

        // (mlme->sme) Send a DeauthenticateConf as a response
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address },
        };
        state.on_mlme_event(deauth_conf, &mut h.context)
    }

    fn expect_set_ctrl_port(
        mlme_stream: &mut MlmeStream,
        bssid: Bssid,
        state: fidl_mlme::ControlledPortState,
    ) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetCtrlPort(req))) => {
            assert_eq!(&req.peer_sta_address, bssid.as_array());
            assert_eq!(req.state, state);
        });
    }

    fn expect_deauth_req(
        mlme_stream: &mut MlmeStream,
        bssid: Bssid,
        reason_code: fidl_ieee80211::ReasonCode,
    ) {
        // (sme->mlme) Expect a DeauthenticateRequest
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::Deauthenticate(req))) => {
            assert_eq!(bssid.as_array(), &req.peer_sta_address);
            assert_eq!(reason_code, req.reason_code);
        });
    }

    #[track_caller]
    fn expect_eapol_req(mlme_stream: &mut MlmeStream, bssid: Bssid) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::Eapol(req))) => {
            assert_eq!(req.src_addr, fake_device_info().sta_addr);
            assert_eq!(&req.dst_addr, bssid.as_array());
            assert_eq!(req.data, Vec::<u8>::from(test_utils::eapol_key_frame()));
        });
    }

    fn expect_set_ptk(mlme_stream: &mut MlmeStream, bssid: Bssid) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetKeys(set_keys_req))) => {
            assert_eq!(set_keys_req.keylist.len(), 1);
            let k = set_keys_req.keylist.first().expect("expect key descriptor");
            assert_eq!(k.key, vec![0xCCu8; test_utils::cipher().tk_bytes().unwrap() as usize]);
            assert_eq!(k.key_id, 0);
            assert_eq!(k.key_type, fidl_mlme::KeyType::Pairwise);
            assert_eq!(&k.address, bssid.as_array());
            assert_eq!(k.rsc, 0);
            assert_eq!(k.cipher_suite_oui, [0x00, 0x0F, 0xAC]);
            assert_eq!(k.cipher_suite_type, fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(4));
        });
    }

    fn expect_set_gtk(mlme_stream: &mut MlmeStream) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetKeys(set_keys_req))) => {
            assert_eq!(set_keys_req.keylist.len(), 1);
            let k = set_keys_req.keylist.first().expect("expect key descriptor");
            assert_eq!(&k.key[..], &test_utils::gtk_bytes()[..]);
            assert_eq!(k.key_id, 2);
            assert_eq!(k.key_type, fidl_mlme::KeyType::Group);
            assert_eq!(k.address, [0xFFu8; 6]);
            assert_eq!(k.rsc, 0);
            assert_eq!(k.cipher_suite_oui, [0x00, 0x0F, 0xAC]);
            assert_eq!(k.cipher_suite_type, fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(4));
        });
    }

    fn expect_set_wpa1_ptk(mlme_stream: &mut MlmeStream, bssid: Bssid) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetKeys(set_keys_req))) => {
            assert_eq!(set_keys_req.keylist.len(), 1);
            let k = set_keys_req.keylist.first().expect("expect key descriptor");
            assert_eq!(k.key, vec![0xCCu8; test_utils::wpa1_cipher().tk_bytes().unwrap() as usize]);
            assert_eq!(k.key_id, 0);
            assert_eq!(k.key_type, fidl_mlme::KeyType::Pairwise);
            assert_eq!(&k.address, bssid.as_array());
            assert_eq!(k.rsc, 0);
            assert_eq!(k.cipher_suite_oui, [0x00, 0x50, 0xF2]);
            assert_eq!(k.cipher_suite_type, fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(2));
        });
    }

    fn expect_set_wpa1_gtk(mlme_stream: &mut MlmeStream) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetKeys(set_keys_req))) => {
            assert_eq!(set_keys_req.keylist.len(), 1);
            let k = set_keys_req.keylist.first().expect("expect key descriptor");
            assert_eq!(&k.key[..], &test_utils::wpa1_gtk_bytes()[..]);
            assert_eq!(k.key_id, 2);
            assert_eq!(k.key_type, fidl_mlme::KeyType::Group);
            assert_eq!(k.address, [0xFFu8; 6]);
            assert_eq!(k.rsc, 0);
            assert_eq!(k.cipher_suite_oui, [0x00, 0x50, 0xF2]);
            assert_eq!(k.cipher_suite_type, fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(2));
        });
    }

    fn connect_command_one() -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let cmd = ConnectCommand {
            bss: Box::new(fake_bss_description!(Open,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [7, 7, 7, 7, 7, 7],
                rssi_dbm: 60,
                snr_db: 30
            )),
            connect_txn_sink,
            protection: Protection::Open,
            authentication: Authentication { protocol: Protocol::Open, credentials: None },
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_two() -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let cmd = ConnectCommand {
            bss: Box::new(
                fake_bss_description!(Open, ssid: Ssid::try_from("bar").unwrap(), bssid: [8, 8, 8, 8, 8, 8]),
            ),
            connect_txn_sink,
            protection: Protection::Open,
            authentication: Authentication { protocol: Protocol::Open, credentials: None },
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_wep() -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let cmd = ConnectCommand {
            bss: Box::new(fake_bss_description!(Wep, ssid: Ssid::try_from("wep").unwrap())),
            connect_txn_sink,
            protection: Protection::Wep(WepKey::Wep40([3; 5])),
            authentication: Authentication { protocol: Protocol::Wep, credentials: None },
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_wpa1(
        supplicant: MockSupplicant,
    ) -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let wpa_ie = make_wpa1_ie();
        let cmd = ConnectCommand {
            bss: Box::new(fake_bss_description!(Wpa1, ssid: Ssid::try_from("wpa1").unwrap())),
            connect_txn_sink,
            protection: Protection::LegacyWpa(Rsna {
                negotiated_protection: NegotiatedProtection::from_legacy_wpa(&wpa_ie)
                    .expect("invalid NegotiatedProtection"),
                supplicant: Box::new(supplicant),
            }),
            authentication: Authentication { protocol: Protocol::Wpa1, credentials: None },
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_wpa2(
        supplicant: MockSupplicant,
    ) -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let bss = fake_bss_description!(Wpa2, ssid: Ssid::try_from("wpa2").unwrap());
        let rsne = Rsne::wpa2_rsne();
        let credentials = Some(Box::new(Credentials::Wpa(
            fidl_security::WpaCredentials::Passphrase("password".into()),
        )));
        let cmd = ConnectCommand {
            bss: Box::new(bss),
            connect_txn_sink,
            protection: Protection::Rsna(Rsna {
                negotiated_protection: NegotiatedProtection::from_rsne(&rsne)
                    .expect("invalid NegotiatedProtection"),
                supplicant: Box::new(supplicant),
            }),
            authentication: Authentication { protocol: Protocol::Wpa2Personal, credentials },
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_wpa3(
        supplicant: MockSupplicant,
    ) -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let bss = fake_bss_description!(Wpa3, ssid: Ssid::try_from("wpa3").unwrap());
        let rsne = Rsne::wpa3_rsne();
        let cmd = ConnectCommand {
            bss: Box::new(bss),
            connect_txn_sink,
            protection: Protection::Rsna(Rsna {
                negotiated_protection: NegotiatedProtection::from_rsne(&rsne)
                    .expect("invalid NegotiatedProtection"),
                supplicant: Box::new(supplicant),
            }),
            authentication: Authentication { protocol: Protocol::Wpa3Personal, credentials: None },
        };
        (cmd, connect_txn_stream)
    }

    fn idle_state() -> ClientState {
        testing::new_state(Idle { cfg: ClientConfig::default() }).into()
    }

    fn assert_idle(state: ClientState) {
        assert_variant!(&state, ClientState::Idle(_));
    }

    fn disconnect(
        mut state: ClientState,
        h: &mut TestHelper,
        reason: fidl_sme::UserDisconnectReason,
    ) -> ClientState {
        let bssid = match &state {
            ClientState::Connecting(state) => state.cmd.bss.bssid,
            ClientState::Associated(state) => state.latest_ap_state.bssid,
            other => panic!("Unexpected state {:?} when disconnecting", other),
        };
        let (mut disconnect_fut, responder) = make_disconnect_request(h);
        state = state.disconnect(&mut h.context, reason, responder);
        assert_variant!(&state, ClientState::Disconnecting(_));
        assert_variant!(h.executor.run_until_stalled(&mut disconnect_fut), Poll::Pending);
        let state = state.on_mlme_event(
            fidl_mlme::MlmeEvent::DeauthenticateConf {
                resp: fidl_mlme::DeauthenticateConfirm { peer_sta_address: bssid.to_array() },
            },
            &mut h.context,
        );
        assert_variant!(h.executor.run_until_stalled(&mut disconnect_fut), Poll::Ready(Ok(())));
        state
    }

    fn connecting_state(cmd: ConnectCommand) -> ClientState {
        testing::new_state(Connecting {
            cfg: ClientConfig::default(),
            cmd,
            protection_ie: None,
            reassociation_loop_count: 0,
        })
        .into()
    }

    fn assert_connecting(state: ClientState, bss: &BssDescription) {
        assert_variant!(&state, ClientState::Connecting(connecting) => {
            assert_eq!(connecting.cmd.bss.as_ref(), bss);
        });
    }

    fn assert_roaming(state: &ClientState) {
        assert_variant!(state, ClientState::Roaming(_));
    }

    fn assert_disconnecting(state: ClientState) {
        assert_variant!(&state, ClientState::Disconnecting(_));
    }

    fn establishing_rsna_state(cmd: ConnectCommand) -> ClientState {
        let auth_method = cmd.protection.rsn_auth_method();
        let rsna = assert_variant!(cmd.protection, Protection::Rsna(rsna) => rsna);
        let link_state = testing::new_state(EstablishingRsna {
            rsna,
            rsna_completion_timeout: None,
            rsna_response_timeout: None,
            rsna_retransmission_timeout: None,
            handshake_complete: false,
            pending_key_ids: Default::default(),
        })
        .into();
        testing::new_state(Associated {
            cfg: ClientConfig::default(),
            latest_ap_state: cmd.bss,
            auth_method,
            connect_txn_sink: cmd.connect_txn_sink,
            last_signal_report_time: zx::MonotonicInstant::ZERO,
            link_state,
            protection_ie: None,
            wmm_param: None,
            last_channel_switch_time: None,
            reassociation_loop_count: 0,
            authentication: cmd.authentication,
        })
        .into()
    }

    fn link_up_state(cmd: ConnectCommand) -> ClientState {
        link_up_state_with_wmm(cmd, None)
    }

    fn link_up_state_with_wmm(cmd: ConnectCommand, wmm_param: Option<ie::WmmParam>) -> ClientState {
        let auth_method = cmd.protection.rsn_auth_method();
        let link_state =
            testing::new_state(LinkUp { protection: cmd.protection, since: now() }).into();
        testing::new_state(Associated {
            cfg: ClientConfig::default(),
            connect_txn_sink: cmd.connect_txn_sink,
            latest_ap_state: cmd.bss,
            auth_method,
            last_signal_report_time: zx::MonotonicInstant::ZERO,
            link_state,
            protection_ie: None,
            wmm_param,
            last_channel_switch_time: None,
            reassociation_loop_count: 0,
            authentication: cmd.authentication,
        })
        .into()
    }

    fn roaming_state(cmd: ConnectCommand, selected_bssid: Bssid) -> ClientState {
        let auth_method = cmd.protection.rsn_auth_method();
        let mut selected_bss = cmd.bss.clone();
        selected_bss.bssid = selected_bssid;
        testing::new_state(Roaming {
            cfg: ClientConfig::default(),
            cmd: ConnectCommand {
                bss: selected_bss,
                connect_txn_sink: cmd.connect_txn_sink,
                protection: cmd.protection,
                authentication: cmd.authentication,
            },
            auth_method,
            protection_ie: None,
        })
        .into()
    }

    fn disconnecting_state(action: PostDisconnectAction) -> ClientState {
        testing::new_state(Disconnecting { cfg: ClientConfig::default(), action, timeout_id: 0 })
            .into()
    }

    fn fake_device_info() -> fidl_mlme::DeviceInfo {
        test_utils::fake_device_info([0, 1, 2, 3, 4, 5].into())
    }
}
