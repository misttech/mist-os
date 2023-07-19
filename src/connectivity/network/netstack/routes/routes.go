// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//go:build !build_with_native_toolchain

package routes

import (
	"errors"
	"fmt"
	"net"
	"sort"
	"strings"

	syslog "go.fuchsia.dev/fuchsia/src/lib/syslog/go"

	"go.fuchsia.dev/fuchsia/src/connectivity/network/netstack/routetypes"
	"go.fuchsia.dev/fuchsia/src/connectivity/network/netstack/sync"
	"go.fuchsia.dev/fuchsia/src/connectivity/network/netstack/util"

	"gvisor.dev/gvisor/pkg/tcpip"
	"gvisor.dev/gvisor/pkg/tcpip/header"
	"gvisor.dev/gvisor/pkg/tcpip/stack"
)

const tag = "routes"

var (
	ErrNoSuchRoute = errors.New("no such route")
	ErrNoSuchNIC   = errors.New("no such NIC")
)

type ExtendedRouteTable []routetypes.ExtendedRoute

func (rt ExtendedRouteTable) String() string {
	var out strings.Builder
	for _, r := range rt {
		fmt.Fprintf(&out, "%s\n", &r)
	}
	return out.String()
}

type RoutingTableChange = routetypes.RoutingTableChange
type SendRoutingTableChangeCb func(RoutingTableChange)

// RoutingChangeSender queues up pending changes to the RouteTable, and
// sends these changes to clients of the `fuchsia.net.routes.Watcher` protocols
// once the events changes are committed into gVisor.
type routingChangeSender struct {
	// A buffer containing accumulated changes to the routing table that have
	// not yet been sent into `RoutingChangesChan`.
	pendingChanges []RoutingTableChange
	// A channel to forward changes in routing state to the clients of
	// fuchsia.net.routes Watcher protocols.
	onChangeCb SendRoutingTableChangeCb
}

// queueChange queues a RoutingTableChange in the RoutingChangeSender.
func (s *routingChangeSender) queueChange(c RoutingTableChange) {
	if s.onChangeCb != nil {
		s.pendingChanges = append(s.pendingChanges, c)
	}
}

// flushChanges drains the pendingChanges by sending each RoutingTableChange
// into the RoutingChangesChan.
func (s *routingChangeSender) flushChanges() {
	if s.onChangeCb == nil {
		return
	}
	for _, c := range s.pendingChanges {
		s.onChangeCb(c)
	}
	s.pendingChanges = nil
}

// RouteTable implements a sorted list of extended routes that is used to build
// the Netstack lib route table.
type RouteTable struct {
	sync.Mutex
	routes ExtendedRouteTable
	sender routingChangeSender
}

func NewRouteTableWithOnChangeCallback(cb SendRoutingTableChangeCb) RouteTable {
	return RouteTable{
		sender: routingChangeSender{
			onChangeCb: cb,
		},
	}
}

// For debugging.
func (rt *RouteTable) dumpLocked() {
	if rt == nil {
		syslog.VLogTf(syslog.TraceVerbosity, tag, "Current Route Table:<nil>")
	} else {
		syslog.VLogTf(syslog.TraceVerbosity, tag, "Current Route Table:\n%s", rt.routes)
	}
}

// HasDefaultRoutes returns whether an interface has default IPv4/IPv6 routes.
func (rt *RouteTable) HasDefaultRouteLocked(nicid tcpip.NICID) (bool, bool) {
	var v4, v6 bool
	for _, er := range rt.routes {
		if er.Route.NIC == nicid && er.Enabled {
			if er.Route.Destination.Equal(header.IPv4EmptySubnet) {
				v4 = true
			} else if er.Route.Destination.Equal(header.IPv6EmptySubnet) {
				v6 = true
			}
		}
	}
	return v4, v6
}

// For testing.
func (rt *RouteTable) Set(r []routetypes.ExtendedRoute) {
	rt.Lock()
	defer rt.Unlock()
	rt.routes = append([]routetypes.ExtendedRoute(nil), r...)
}

func (rt *RouteTable) AddRouteLocked(route tcpip.Route, prf routetypes.Preference, metric routetypes.Metric, tracksInterface bool, dynamic bool, enabled bool, overwriteIfAlreadyExists bool) (newlyAdded bool) {
	syslog.VLogTf(syslog.DebugVerbosity, tag, "RouteTable:Adding route %s with prf=%d metric=%d, trackIf=%t, dynamic=%t, enabled=%t, overwriteIfAlreadyExists=%t", route, prf, metric, tracksInterface, dynamic, enabled, overwriteIfAlreadyExists)

	newlyAdded = true
	// First check if the route already exists, and remove it.
	for i, er := range rt.routes {
		if er.Route == route {
			if !overwriteIfAlreadyExists {
				return false
			}

			rt.routes = append(rt.routes[:i], rt.routes[i+1:]...)
			rt.sender.queueChange(RoutingTableChange{
				Change: routetypes.RouteRemoved,
				Route:  er,
			})
			newlyAdded = false
			break
		}
	}

	newEr := routetypes.ExtendedRoute{
		Route:                 route,
		Prf:                   prf,
		Metric:                metric,
		MetricTracksInterface: tracksInterface,
		Dynamic:               dynamic,
		Enabled:               enabled,
	}

	// Find the target position for the new route in the table so it remains
	// sorted.
	targetIdx := sort.Search(len(rt.routes), func(i int) bool {
		return Less(&newEr, &rt.routes[i])
	})
	// Extend the table by adding the new route at the end, then move it into its
	// proper place.
	rt.routes = append(rt.routes, newEr)
	if targetIdx < len(rt.routes)-1 {
		copy(rt.routes[targetIdx+1:], rt.routes[targetIdx:])
		rt.routes[targetIdx] = newEr
	}

	rt.dumpLocked()

	rt.sender.queueChange(RoutingTableChange{
		Change: routetypes.RouteAdded,
		Route:  newEr,
	})

	return newlyAdded
}

// AddRoute inserts the given route to the table in a sorted fashion. If the
// route already exists, it simply updates that route's preference, metric,
// dynamic, and enabled fields.
func (rt *RouteTable) AddRoute(route tcpip.Route, prf routetypes.Preference, metric routetypes.Metric, tracksInterface bool, dynamic bool, enabled bool) {
	rt.Lock()
	defer rt.Unlock()

	rt.AddRouteLocked(route, prf, metric, tracksInterface, dynamic, enabled, true /* overwriteIfAlreadyExists */)
}

func (rt *RouteTable) DelRouteLocked(route tcpip.Route) []routetypes.ExtendedRoute {
	return rt.delRouteLocked(route, false)
}

func (rt *RouteTable) DelRouteIfDynamicLocked(route tcpip.Route) []routetypes.ExtendedRoute {
	return rt.delRouteLocked(route, true)
}

func (rt *RouteTable) delRouteLocked(route tcpip.Route, onlyDeleteIfDynamic bool) []routetypes.ExtendedRoute {
	syslog.VLogTf(syslog.DebugVerbosity, tag, "RouteTable:Deleting route %s", route)

	var routesDeleted []routetypes.ExtendedRoute
	oldTable := rt.routes
	rt.routes = oldTable[:0]
	if cap(oldTable) > 2*len(oldTable) {
		// Remove excess route table capacity instead of reusing old capacity.
		rt.routes = make([]routetypes.ExtendedRoute, 0, len(oldTable))
	}
	for _, er := range oldTable {
		if er.Route.Destination == route.Destination && er.Route.NIC == route.NIC {
			// Match any route if Gateway is empty.
			if (route.Gateway.Len() == 0 ||
				er.Route.Gateway == route.Gateway) && (!onlyDeleteIfDynamic || er.Dynamic) {
				routesDeleted = append(routesDeleted, er)
				continue
			}
		}
		// Not matched, remains in the route table.
		rt.routes = append(rt.routes, er)
	}

	if len(routesDeleted) == 0 {
		return nil
	}

	// Zero out unused entries in the routes slice so that they can be garbage collected.
	deadRoutes := rt.routes[len(rt.routes):cap(rt.routes)]
	for i := range deadRoutes {
		deadRoutes[i] = routetypes.ExtendedRoute{}
	}

	rt.dumpLocked()

	for _, er := range routesDeleted {
		rt.sender.queueChange(RoutingTableChange{
			Change: routetypes.RouteRemoved,
			Route:  er,
		})
	}

	return routesDeleted
}

// DelRoute removes matching routes from the route table, returning them.
func (rt *RouteTable) DelRoute(route tcpip.Route) []routetypes.ExtendedRoute {
	rt.Lock()
	defer rt.Unlock()

	return rt.DelRouteLocked(route)
}

// GetExtendedRouteTable returns a copy of the current extended route table.
func (rt *RouteTable) GetExtendedRouteTable() ExtendedRouteTable {
	rt.Lock()
	defer rt.Unlock()

	rt.dumpLocked()

	return append([]routetypes.ExtendedRoute(nil), rt.routes...)
}

// UpdateStack updates stack with the current route table.
func (rt *RouteTable) UpdateStackLocked(stack *stack.Stack, onUpdateSucceeded func()) {
	t := make([]tcpip.Route, 0, len(rt.routes))
	for _, er := range rt.routes {
		if er.Enabled {
			t = append(t, er.Route)
		}
	}
	stack.SetRouteTable(t)

	_ = syslog.VLogTf(syslog.DebugVerbosity, tag, "UpdateStack route table: %+v", t)
	onUpdateSucceeded()

	// Notify clients of `fuchsia.net.routes/Watcher` of the changes.
	rt.sender.flushChanges()
}

// UpdateStack updates stack with the current route table.
func (rt *RouteTable) UpdateStack(stack *stack.Stack, onUpdateSucceeded func()) {
	rt.Lock()
	defer rt.Unlock()

	rt.UpdateStackLocked(stack, onUpdateSucceeded)
}

func (rt *RouteTable) UpdateRoutesByInterfaceLocked(nicid tcpip.NICID, action routetypes.Action) {
	syslog.VLogTf(syslog.DebugVerbosity, tag, "RouteTable:Update route table for routes to nic-%d with action:%d", nicid, action)

	oldTable := rt.routes
	rt.routes = oldTable[:0]
	if cap(oldTable) > 2*len(oldTable) {
		// Remove excess route table capacity instead of reusing old capacity.
		rt.routes = make([]routetypes.ExtendedRoute, 0, len(oldTable))
	}
	for _, er := range oldTable {
		if er.Route.NIC == nicid {
			switch action {
			case routetypes.ActionDeleteAll:
				rt.sender.queueChange(RoutingTableChange{
					Change: routetypes.RouteRemoved,
					Route:  er,
				})
				continue // delete
			case routetypes.ActionDeleteDynamic:
				if er.Dynamic {
					rt.sender.queueChange(RoutingTableChange{
						Change: routetypes.RouteRemoved,
						Route:  er,
					})
					continue // delete
				}
			case routetypes.ActionDisableStatic:
				if !er.Dynamic {
					er.Enabled = false
				}
			case routetypes.ActionEnableStatic:
				if !er.Dynamic {
					er.Enabled = true
				}
			}
		}
		// Keep.
		rt.routes = append(rt.routes, er)
	}

	// Zero out unused entries in the routes slice so that they can be garbage collected.
	deadRoutes := rt.routes[len(rt.routes):cap(rt.routes)]
	for i := range deadRoutes {
		deadRoutes[i] = routetypes.ExtendedRoute{}
	}

	rt.sortRouteTableLocked()

	rt.dumpLocked()
}

// UpdateRoutesByInterface applies an action to the routes pointing to an interface.
func (rt *RouteTable) UpdateRoutesByInterface(nicid tcpip.NICID, action routetypes.Action) {
	rt.Lock()
	defer rt.Unlock()

	rt.UpdateRoutesByInterfaceLocked(nicid, action)
}

// FindNIC returns the NIC-ID that the given address is routed on. This requires
// an exact route match, i.e. no default route.
func (rt *RouteTable) FindNIC(addr tcpip.Address) (tcpip.NICID, error) {
	rt.Lock()
	defer rt.Unlock()

	for _, er := range rt.routes {
		// Ignore default routes.
		if util.IsAny(er.Route.Destination.ID()) {
			continue
		}
		if er.Match(addr) && er.Route.NIC > 0 {
			return er.Route.NIC, nil
		}
	}
	return 0, ErrNoSuchNIC
}

func (rt *RouteTable) sortRouteTableLocked() {
	sort.SliceStable(rt.routes, func(i, j int) bool {
		return Less(&rt.routes[i], &rt.routes[j])
	})
}

// Less compares two routes and returns which one should appear earlier in the
// route table.
func Less(ei, ej *routetypes.ExtendedRoute) bool {
	ri, rj := ei.Route, ej.Route
	riDest, rjDest := ri.Destination.ID(), rj.Destination.ID()

	// Loopback routes before non-loopback ones.
	// (as a workaround for github.com/google/gvisor/issues/1169).
	if riIsLoop, rjIsLoop := net.IP(riDest.AsSlice()).IsLoopback(), net.IP(rjDest.AsSlice()).IsLoopback(); riIsLoop != rjIsLoop {
		return riIsLoop
	}

	// Non-default before default one.
	if riAny, rjAny := util.IsAny(riDest), util.IsAny(rjDest); riAny != rjAny {
		return !riAny
	}

	// IPv4 before IPv6 (arbitrary choice).
	if riLen, rjLen := riDest.Len(), rjDest.Len(); riLen != rjLen {
		return riLen == header.IPv4AddressSize
	}

	// Longer prefix wins.
	if riPrefix, rjPrefix := ri.Destination.Prefix(), rj.Destination.Prefix(); riPrefix != rjPrefix {
		return riPrefix > rjPrefix
	}

	// On-link wins.
	if riOnLink, rjOnLink := ri.Gateway.Len() == 0, rj.Gateway.Len() == 0; riOnLink != rjOnLink {
		return riOnLink
	}

	// Higher preference wins.
	if ei.Prf != ej.Prf {
		return ei.Prf > ej.Prf
	}

	// Lower metrics wins.
	if ei.Metric != ej.Metric {
		return ei.Metric < ej.Metric
	}

	// Everything that matters is the same. At this point we still need a
	// deterministic way to tie-break. First go by destination IPs (lower wins),
	// finally use the NIC.
	riDestAsSlice := riDest.AsSlice()
	rjDestAsSlice := rjDest.AsSlice()
	for i := 0; i < riDest.Len(); i++ {
		if riDestAsSlice[i] != rjDestAsSlice[i] {
			return riDestAsSlice[i] < rjDestAsSlice[i]
		}
	}

	// Same prefix and destination IPs (e.g. loopback IPs), use NIC as final
	// tie-breaker.
	return ri.NIC < rj.NIC
}
