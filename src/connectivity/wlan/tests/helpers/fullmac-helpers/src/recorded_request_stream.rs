// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    fidl_fuchsia_wlan_fullmac::{self as fidl_fullmac, WlanFullmacImplBridgeRequest},
    futures::StreamExt,
};

// Wrapper type for WlanFullmacImplBridgeRequest types without the responder.
#[derive(Clone, Debug, PartialEq)]
pub enum FullmacRequest {
    Stop,
    Query,
    QueryMacSublayerSupport,
    QuerySecuritySupport,
    StartScan(fidl_fullmac::WlanFullmacImplBaseStartScanRequest),
    Connect(fidl_fullmac::WlanFullmacImplBaseConnectRequest),
    Reconnect(fidl_fullmac::WlanFullmacImplBaseReconnectRequest),
    AuthResp(fidl_fullmac::WlanFullmacImplBaseAuthRespRequest),
    Deauth(fidl_fullmac::WlanFullmacImplBaseDeauthRequest),
    AssocResp(fidl_fullmac::WlanFullmacImplBaseAssocRespRequest),
    Disassoc(fidl_fullmac::WlanFullmacImplBaseDisassocRequest),
    Reset(fidl_fullmac::WlanFullmacImplBaseResetRequest),
    StartBss(fidl_fullmac::WlanFullmacImplBaseStartBssRequest),
    StopBss(fidl_fullmac::WlanFullmacImplBaseStopBssRequest),
    SetKeysReq(fidl_fullmac::WlanFullmacSetKeysReq),
    DelKeysReq(fidl_fullmac::WlanFullmacDelKeysReq),
    EapolTx(fidl_fullmac::WlanFullmacImplBaseEapolTxRequest),
    GetIfaceCounterStats,
    GetIfaceHistogramStats,
    SaeHandshakeResp(fidl_fullmac::WlanFullmacSaeHandshakeResp),
    SaeFrameTx(fidl_fullmac::WlanFullmacSaeFrame),
    WmmStatusReq,
    SetMulticastPromisc(bool),
    OnLinkStateChanged(bool),

    // Note: WlanFullmacImpl::Start has a channel as an argument, but we don't keep the channel
    // here.
    Start,
}

/// A wrapper around WlanFullmacImplBridgeRequestStream that records each handled request in its
/// |history|. Users of this type should not access |request_stream| directly; instead, use
/// RecordedRequestStream::handle_request.
pub struct RecordedRequestStream {
    request_stream: fidl_fullmac::WlanFullmacImplBridgeRequestStream,
    history: Vec<FullmacRequest>,
}

impl RecordedRequestStream {
    pub fn new(request_stream: fidl_fullmac::WlanFullmacImplBridgeRequestStream) -> Self {
        Self { request_stream, history: Vec::new() }
    }

    pub fn history(&self) -> &[FullmacRequest] {
        &self.history[..]
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Retrieves a single request from the request stream.
    /// This records the request type in its history (copying the request payload out if one
    /// exists) before returning it.
    pub async fn next(&mut self) -> fidl_fullmac::WlanFullmacImplBridgeRequest {
        let request = self
            .request_stream
            .next()
            .await
            .unwrap()
            .expect("Could not get next request in fullmac request stream");
        match &request {
            WlanFullmacImplBridgeRequest::Stop { .. } => self.history.push(FullmacRequest::Stop),
            WlanFullmacImplBridgeRequest::Query { .. } => self.history.push(FullmacRequest::Query),
            WlanFullmacImplBridgeRequest::QueryMacSublayerSupport { .. } => {
                self.history.push(FullmacRequest::QueryMacSublayerSupport)
            }
            WlanFullmacImplBridgeRequest::QuerySecuritySupport { .. } => {
                self.history.push(FullmacRequest::QuerySecuritySupport)
            }
            WlanFullmacImplBridgeRequest::StartScan { payload, .. } => {
                self.history.push(FullmacRequest::StartScan(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::Connect { payload, .. } => {
                self.history.push(FullmacRequest::Connect(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::Reconnect { payload, .. } => {
                self.history.push(FullmacRequest::Reconnect(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::AuthResp { payload, .. } => {
                self.history.push(FullmacRequest::AuthResp(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::Deauth { payload, .. } => {
                self.history.push(FullmacRequest::Deauth(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::AssocResp { payload, .. } => {
                self.history.push(FullmacRequest::AssocResp(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::Disassoc { payload, .. } => {
                self.history.push(FullmacRequest::Disassoc(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::Reset { payload, .. } => {
                self.history.push(FullmacRequest::Reset(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::StartBss { payload, .. } => {
                self.history.push(FullmacRequest::StartBss(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::StopBss { payload, .. } => {
                self.history.push(FullmacRequest::StopBss(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::SetKeysReq { req, .. } => {
                self.history.push(FullmacRequest::SetKeysReq(req.clone()))
            }
            WlanFullmacImplBridgeRequest::DelKeysReq { req, .. } => {
                self.history.push(FullmacRequest::DelKeysReq(req.clone()))
            }
            WlanFullmacImplBridgeRequest::EapolTx { payload, .. } => {
                self.history.push(FullmacRequest::EapolTx(payload.clone()))
            }
            WlanFullmacImplBridgeRequest::GetIfaceCounterStats { .. } => {
                self.history.push(FullmacRequest::GetIfaceCounterStats)
            }
            WlanFullmacImplBridgeRequest::GetIfaceHistogramStats { .. } => {
                self.history.push(FullmacRequest::GetIfaceHistogramStats)
            }
            WlanFullmacImplBridgeRequest::SaeHandshakeResp { resp, .. } => {
                self.history.push(FullmacRequest::SaeHandshakeResp(resp.clone()))
            }
            WlanFullmacImplBridgeRequest::SaeFrameTx { frame, .. } => {
                self.history.push(FullmacRequest::SaeFrameTx(frame.clone()))
            }
            WlanFullmacImplBridgeRequest::WmmStatusReq { .. } => {
                self.history.push(FullmacRequest::WmmStatusReq)
            }
            WlanFullmacImplBridgeRequest::SetMulticastPromisc { enable, .. } => {
                self.history.push(FullmacRequest::SetMulticastPromisc(*enable))
            }
            WlanFullmacImplBridgeRequest::OnLinkStateChanged { online, .. } => {
                self.history.push(FullmacRequest::OnLinkStateChanged(*online))
            }
            WlanFullmacImplBridgeRequest::Start { .. } => self.history.push(FullmacRequest::Start),

            _ => panic!("Unrecognized Fullmac request {:?}", request),
        }
        request
    }
}
