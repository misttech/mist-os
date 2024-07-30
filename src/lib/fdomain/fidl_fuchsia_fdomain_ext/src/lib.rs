// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_fdomain as proto;

/// Means a value is convertible to an FDomain `Signals` type. This is a type
/// analogous to `zx::Signals` but which versions differently to insulate
/// FDomain from Zircon changes.
pub trait AsFDomainSignals {
    /// Convert to a `fidl_fuchsia_fdomain::Signals`. The object type the signal
    /// will be used on is required to make this conversion unambiguous.
    ///
    /// If this method returns `None`, either the object type was a type of
    /// object FDomain doesn't yet support, or some of the signals weren't
    /// recognized as either general signals or signals for that type.
    fn as_fdomain_signals(self, ty: proto::ObjType) -> Option<proto::Signals>;
}

impl AsFDomainSignals for proto::Signals {
    fn as_fdomain_signals(self, _ty: proto::ObjType) -> Option<proto::Signals> {
        Some(self)
    }
}

impl AsFDomainSignals for fidl::Signals {
    fn as_fdomain_signals(mut self, ty: proto::ObjType) -> Option<proto::Signals> {
        let mut ret = proto::Signals { typed: None, general: proto::General::empty() };

        for (sig, proto_sig) in [
            (fidl::Signals::HANDLE_CLOSED, proto::General::HANDLE_CLOSED),
            (fidl::Signals::USER_0, proto::General::USER_0),
            (fidl::Signals::USER_1, proto::General::USER_1),
            (fidl::Signals::USER_2, proto::General::USER_2),
            (fidl::Signals::USER_3, proto::General::USER_3),
            (fidl::Signals::USER_4, proto::General::USER_4),
            (fidl::Signals::USER_5, proto::General::USER_5),
            (fidl::Signals::USER_6, proto::General::USER_6),
            (fidl::Signals::USER_7, proto::General::USER_7),
        ] {
            if self.contains(sig) {
                self.remove(sig);
                ret.general |= proto_sig;
            }
        }

        match ty {
            proto::ObjType::Channel => {
                let mut sigs = proto::ChannelSignals::empty();
                for (sig, proto_sig) in [
                    (fidl::Signals::CHANNEL_READABLE, proto::ChannelSignals::READABLE),
                    (fidl::Signals::CHANNEL_WRITABLE, proto::ChannelSignals::WRITABLE),
                    (fidl::Signals::CHANNEL_PEER_CLOSED, proto::ChannelSignals::PEER_CLOSED),
                ] {
                    if self.contains(sig) {
                        self.remove(sig);
                        sigs |= proto_sig;
                    }
                }
                ret.typed = Some(Box::new(proto::Typed::Channel(sigs)));
            }
            proto::ObjType::Socket => {
                let mut sigs = proto::SocketSignals::empty();
                for (sig, proto_sig) in [
                    (fidl::Signals::SOCKET_READABLE, proto::SocketSignals::READABLE),
                    (fidl::Signals::SOCKET_WRITABLE, proto::SocketSignals::WRITABLE),
                    (fidl::Signals::SOCKET_PEER_CLOSED, proto::SocketSignals::PEER_CLOSED),
                    (
                        fidl::Signals::SOCKET_PEER_WRITE_DISABLED,
                        proto::SocketSignals::PEER_WRITE_DISABLED,
                    ),
                    (fidl::Signals::SOCKET_READ_THRESHOLD, proto::SocketSignals::READ_THRESHOLD),
                    (fidl::Signals::SOCKET_WRITE_DISABLED, proto::SocketSignals::WRITE_DISABLED),
                    (fidl::Signals::SOCKET_WRITE_THRESHOLD, proto::SocketSignals::WRITE_THRESHOLD),
                ] {
                    if self.contains(sig) {
                        self.remove(sig);
                        sigs |= proto_sig;
                    }
                }
                ret.typed = Some(Box::new(proto::Typed::Socket(sigs)));
            }
            proto::ObjType::Eventpair => {
                let mut sigs = proto::EventPairSignals::empty();
                for (sig, proto_sig) in [
                    (fidl::Signals::EVENTPAIR_PEER_CLOSED, proto::EventPairSignals::PEER_CLOSED),
                    (fidl::Signals::EVENTPAIR_SIGNALED, proto::EventPairSignals::SIGNALED),
                ] {
                    if self.contains(sig) {
                        self.remove(sig);
                        sigs |= proto_sig;
                    }
                }
                ret.typed = Some(Box::new(proto::Typed::EventPair(sigs)));
            }
            proto::ObjType::Event => {
                if self.contains(fidl::Signals::EVENT_SIGNALED) {
                    self.remove(fidl::Signals::EVENT_SIGNALED);
                    ret.typed = Some(Box::new(proto::Typed::Event(proto::EventSignals::SIGNALED)));
                }
            }
            _ => (),
        }

        if self.is_empty() {
            Some(ret)
        } else {
            None
        }
    }
}

/// Means a value is convertible to an FDomain `ObjType` type. This is a type
/// analogous to `zx::ObjectType` but which versions differently to insulate
/// FDomain from Zircon changes.
pub trait AsFDomainObjectType {
    /// Convert to a `fidl_fuchsia_fdomain::ObjType`.
    ///
    /// If this method returns `None`, the object type was a type of object
    /// FDomain doesn't understand.
    fn as_fdomain_object_type(self) -> Option<proto::ObjType>;
}

impl AsFDomainObjectType for proto::ObjType {
    fn as_fdomain_object_type(self) -> Option<proto::ObjType> {
        Some(self)
    }
}

impl AsFDomainObjectType for fidl::ObjectType {
    fn as_fdomain_object_type(self) -> Option<proto::ObjType> {
        match self {
            fidl::ObjectType::NONE => Some(proto::ObjType::None),
            fidl::ObjectType::PROCESS => Some(proto::ObjType::Process),
            fidl::ObjectType::THREAD => Some(proto::ObjType::Thread),
            fidl::ObjectType::VMO => Some(proto::ObjType::Vmo),
            fidl::ObjectType::CHANNEL => Some(proto::ObjType::Channel),
            fidl::ObjectType::EVENT => Some(proto::ObjType::Event),
            fidl::ObjectType::PORT => Some(proto::ObjType::Port),
            fidl::ObjectType::INTERRUPT => Some(proto::ObjType::Interrupt),
            fidl::ObjectType::PCI_DEVICE => Some(proto::ObjType::PciDevice),
            fidl::ObjectType::DEBUGLOG => Some(proto::ObjType::Debuglog),
            fidl::ObjectType::SOCKET => Some(proto::ObjType::Socket),
            fidl::ObjectType::RESOURCE => Some(proto::ObjType::Resource),
            fidl::ObjectType::EVENTPAIR => Some(proto::ObjType::Eventpair),
            fidl::ObjectType::JOB => Some(proto::ObjType::Job),
            fidl::ObjectType::VMAR => Some(proto::ObjType::Vmar),
            fidl::ObjectType::FIFO => Some(proto::ObjType::Fifo),
            fidl::ObjectType::GUEST => Some(proto::ObjType::Guest),
            fidl::ObjectType::VCPU => Some(proto::ObjType::Vcpu),
            fidl::ObjectType::TIMER => Some(proto::ObjType::Timer),
            fidl::ObjectType::IOMMU => Some(proto::ObjType::Iommu),
            fidl::ObjectType::BTI => Some(proto::ObjType::Bti),
            fidl::ObjectType::PROFILE => Some(proto::ObjType::Profile),
            fidl::ObjectType::PMT => Some(proto::ObjType::Pmt),
            fidl::ObjectType::SUSPEND_TOKEN => Some(proto::ObjType::SuspendToken),
            fidl::ObjectType::PAGER => Some(proto::ObjType::Pager),
            fidl::ObjectType::EXCEPTION => Some(proto::ObjType::Exception),
            fidl::ObjectType::CLOCK => Some(proto::ObjType::Clock),
            fidl::ObjectType::STREAM => Some(proto::ObjType::Stream),
            fidl::ObjectType::MSI => Some(proto::ObjType::Msi),
            fidl::ObjectType::IOB => Some(proto::ObjType::Iob),
            _ => None,
        }
    }
}

/// Means a value is convertible to an FDomain `Rights` type. This is a type
/// analogous to `zx::Rights` but which versions differently to insulate FDomain
/// from Zircon changes.
pub trait AsFDomainRights {
    /// Convert to a `fidl_fuchsia_fdomain::Rights`.
    ///
    /// If this method returns `None`, the object type was a type of object
    /// FDomain doesn't understand.
    fn as_fdomain_rights(self) -> Option<proto::Rights>;

    /// Convert to a `fidl_fuchsia_fdomain::Rights`.
    ///
    /// If any rights aren't understood by FDomain, those rights are silently
    /// omitted.
    fn as_fdomain_rights_truncate(self) -> proto::Rights;
}

impl AsFDomainRights for proto::Rights {
    fn as_fdomain_rights(self) -> Option<proto::Rights> {
        Some(self)
    }
    fn as_fdomain_rights_truncate(self) -> proto::Rights {
        self
    }
}

impl AsFDomainRights for fidl::Rights {
    fn as_fdomain_rights(mut self) -> Option<proto::Rights> {
        let ret = do_as_fdomain_rights(&mut self);

        if self.is_empty() {
            Some(ret)
        } else {
            None
        }
    }

    fn as_fdomain_rights_truncate(mut self) -> proto::Rights {
        do_as_fdomain_rights(&mut self)
    }
}

fn do_as_fdomain_rights(rights: &mut fidl::Rights) -> proto::Rights {
    let mut ret = proto::Rights::empty();
    for (fidl_right, proto_right) in [
        (fidl::Rights::DUPLICATE, proto::Rights::DUPLICATE),
        (fidl::Rights::TRANSFER, proto::Rights::TRANSFER),
        (fidl::Rights::READ, proto::Rights::READ),
        (fidl::Rights::WRITE, proto::Rights::WRITE),
        (fidl::Rights::EXECUTE, proto::Rights::EXECUTE),
        (fidl::Rights::MAP, proto::Rights::MAP),
        (fidl::Rights::GET_PROPERTY, proto::Rights::GET_PROPERTY),
        (fidl::Rights::SET_PROPERTY, proto::Rights::SET_PROPERTY),
        (fidl::Rights::ENUMERATE, proto::Rights::ENUMERATE),
        (fidl::Rights::DESTROY, proto::Rights::DESTROY),
        (fidl::Rights::SET_POLICY, proto::Rights::SET_POLICY),
        (fidl::Rights::GET_POLICY, proto::Rights::GET_POLICY),
        (fidl::Rights::SIGNAL, proto::Rights::SIGNAL),
        (fidl::Rights::SIGNAL_PEER, proto::Rights::SIGNAL_PEER),
        (fidl::Rights::WAIT, proto::Rights::WAIT),
        (fidl::Rights::INSPECT, proto::Rights::INSPECT),
        (fidl::Rights::MANAGE_JOB, proto::Rights::MANAGE_JOB),
        (fidl::Rights::MANAGE_PROCESS, proto::Rights::MANAGE_PROCESS),
        (fidl::Rights::MANAGE_THREAD, proto::Rights::MANAGE_THREAD),
        (fidl::Rights::APPLY_PROFILE, proto::Rights::APPLY_PROFILE),
        (fidl::Rights::MANAGE_SOCKET, proto::Rights::MANAGE_SOCKET),
        (fidl::Rights::OP_CHILDREN, proto::Rights::OP_CHILDREN),
        (fidl::Rights::RESIZE, proto::Rights::RESIZE),
        (fidl::Rights::ATTACH_VMO, proto::Rights::ATTACH_VMO),
        (fidl::Rights::MANAGE_VMO, proto::Rights::MANAGE_VMO),
        (fidl::Rights::SAME_RIGHTS, proto::Rights::SAME_RIGHTS),
    ] {
        if rights.contains(fidl_right) {
            rights.remove(fidl_right);
            ret |= proto_right;
        }
    }

    ret
}

/// Convert an FDomain protocol Signals object to fidl/zircon Signals. This
/// removes signals from the argument and places them in the return value. If
/// the argument is not an empty signal set afterward, then not all signals
/// could be converted.
pub fn proto_signals_to_fidl_take(sigs: &mut proto::Signals) -> fidl::Signals {
    let mut ret = fidl::Signals::empty();

    for (proto, local) in [
        (proto::General::HANDLE_CLOSED, fidl::Signals::HANDLE_CLOSED),
        (proto::General::USER_0, fidl::Signals::USER_0),
        (proto::General::USER_1, fidl::Signals::USER_1),
        (proto::General::USER_2, fidl::Signals::USER_2),
        (proto::General::USER_3, fidl::Signals::USER_3),
        (proto::General::USER_4, fidl::Signals::USER_4),
        (proto::General::USER_5, fidl::Signals::USER_5),
        (proto::General::USER_6, fidl::Signals::USER_6),
        (proto::General::USER_7, fidl::Signals::USER_7),
    ] {
        if sigs.general.contains(proto) {
            sigs.general.remove(proto);
            ret |= local;
        }
    }

    match sigs.typed.take().map(|x| *x) {
        Some(proto::Typed::Channel(mut channel_sigs)) => {
            for (proto, local) in [
                (proto::ChannelSignals::READABLE, fidl::Signals::CHANNEL_READABLE),
                (proto::ChannelSignals::WRITABLE, fidl::Signals::CHANNEL_WRITABLE),
                (proto::ChannelSignals::PEER_CLOSED, fidl::Signals::CHANNEL_PEER_CLOSED),
            ] {
                if channel_sigs.contains(proto) {
                    channel_sigs.remove(proto);
                    ret |= local;
                }
            }
        }
        Some(proto::Typed::Socket(mut socket_sigs)) => {
            for (proto, local) in [
                (proto::SocketSignals::READABLE, fidl::Signals::SOCKET_READABLE),
                (proto::SocketSignals::WRITABLE, fidl::Signals::SOCKET_WRITABLE),
                (proto::SocketSignals::PEER_CLOSED, fidl::Signals::SOCKET_PEER_CLOSED),
                (
                    proto::SocketSignals::PEER_WRITE_DISABLED,
                    fidl::Signals::SOCKET_PEER_WRITE_DISABLED,
                ),
                (proto::SocketSignals::WRITE_DISABLED, fidl::Signals::SOCKET_WRITE_DISABLED),
                (proto::SocketSignals::READ_THRESHOLD, fidl::Signals::SOCKET_READ_THRESHOLD),
                (proto::SocketSignals::WRITE_THRESHOLD, fidl::Signals::SOCKET_WRITE_THRESHOLD),
            ] {
                if socket_sigs.contains(proto) {
                    socket_sigs.remove(proto);
                    ret |= local;
                }
            }
        }
        Some(proto::Typed::Event(mut event_sigs)) => {
            if event_sigs.contains(proto::EventSignals::SIGNALED) {
                event_sigs.remove(proto::EventSignals::SIGNALED);
                ret |= fidl::Signals::EVENT_SIGNALED;
            }
        }
        Some(proto::Typed::EventPair(mut eventpair_sigs)) => {
            if eventpair_sigs.contains(proto::EventPairSignals::SIGNALED) {
                eventpair_sigs.remove(proto::EventPairSignals::SIGNALED);
                ret |= fidl::Signals::EVENT_SIGNALED;
            }
            if eventpair_sigs.contains(proto::EventPairSignals::PEER_CLOSED) {
                eventpair_sigs.remove(proto::EventPairSignals::PEER_CLOSED);
                ret |= fidl::Signals::EVENTPAIR_PEER_CLOSED;
            }
        }
        Some(s) => {
            sigs.typed = Some(Box::new(s));
        }
        None => (),
    }

    ret
}
