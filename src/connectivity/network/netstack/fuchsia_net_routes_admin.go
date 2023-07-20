// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//go:build !build_with_native_toolchain

package netstack

import (
	"context"
	"errors"
	"fmt"
	"syscall/zx/fidl"

	"go.fuchsia.dev/fuchsia/src/connectivity/network/netstack/fidlconv"
	"go.fuchsia.dev/fuchsia/src/connectivity/network/netstack/routes"
	"go.fuchsia.dev/fuchsia/src/connectivity/network/netstack/routetypes"
	"go.fuchsia.dev/fuchsia/src/lib/component"
	syslog "go.fuchsia.dev/fuchsia/src/lib/syslog/go"

	fuchsianet "fidl/fuchsia/net"
	"fidl/fuchsia/net/interfaces/admin"
	fnetRoutes "fidl/fuchsia/net/routes"
	routesAdmin "fidl/fuchsia/net/routes/admin"
)

const (
	routeSetV4Name  = "fuchsia.net.routes.admin/RouteSetV4"
	routeSetV6Name  = "fuchsia.net.routes.admin/RouteSetV6"
	routesAdminName = "fuchsia.net.routes.admin"
)

type routeSet[A fidlconv.IpAddress] struct {
	ns *Netstack
	id routetypes.RouteSetId
}

func makeRouteSet[A fidlconv.IpAddress](ns *Netstack) routeSet[A] {
	return routeSet[A]{
		ns: ns,
	}
}

// addRoute adds a route to the routeSet and returns whether the route is new
// to the routeSet.
func (r *routeSet[A]) addRoute(route fidlconv.Route[A]) (bool, error) {
	gvisorRoute, err := route.GVisorRoute()
	if err != nil {
		return false, err
	}

	metric, err := func() (routetypes.Metric, error) {
		switch route.Properties.Metric.Which() {
		case fnetRoutes.SpecifiedMetricExplicitMetric:
			return routetypes.Metric(route.Properties.Metric.ExplicitMetric), nil
		case fnetRoutes.SpecifiedMetricInheritedFromInterface:
			return metricNotSet, nil
		default:
			return metricNotSet, fmt.Errorf("unknown metric type: %+v", route.Properties.Metric)
		}
	}()

	if err != nil {
		return false, err
	}

	addResult, err := r.ns.AddRoute(gvisorRoute, metric, false /* dynamic */, false /* replaceMatchingGvisorRoutes */, &r.id)
	return addResult.NewlyAddedToSet, err
}

// removesRoute removes a route from the routeSet and returns whether the route
// was present in the routeSet.
func (r *routeSet[A]) removeRoute(route fidlconv.Route[A]) (bool, error) {
	gvisorRoute, err := route.GVisorRoute()
	if err != nil {
		return false, err
	}

	metric, tracksInterface, err := func() (routetypes.Metric, bool, error) {
		switch route.Properties.Metric.Which() {
		case fnetRoutes.SpecifiedMetricExplicitMetric:
			return routetypes.Metric(route.Properties.Metric.ExplicitMetric), false, nil
		case fnetRoutes.SpecifiedMetricInheritedFromInterface:
			return metricNotSet, true, nil
		default:
			return metricNotSet, false, fmt.Errorf("unknown metric type: %+v", route.Properties.Metric)
		}
	}()

	if err != nil {
		return false, err
	}

	removeResult, err := r.ns.DelRouteExactMatch(gvisorRoute, metric, tracksInterface, &r.id)
	return removeResult.NewlyRemovedFromSet, err
}

// close deletes the routeSet and decrements the routing table's reference count
// for every route in the route set.
func (r *routeSet[A]) close() {
	r.ns.DelRouteSet(&r.id)
}

var _ routesAdmin.SetProviderV4WithCtx = (*routesAdminSetProviderV4Impl)(nil)

type routesAdminSetProviderV4Impl struct {
	ns *Netstack
}

func (impl *routesAdminSetProviderV4Impl) NewRouteSet(ctx_ fidl.Context, request routesAdmin.RouteSetV4WithCtxInterfaceRequest) error {
	ctx, cancel := context.WithCancel(context.Background())

	go func() {
		defer cancel()

		setImpl := routeSetV4Impl{
			routeSet: makeRouteSet[fuchsianet.Ipv4Address](impl.ns),
		}
		defer setImpl.routeSet.close()

		component.Serve(
			ctx,
			&routesAdmin.RouteSetV4WithCtxStub{Impl: &setImpl},
			request.Channel,
			component.ServeOptions{
				Concurrent: true,
				OnError: func(err error) {
					_ = syslog.ErrorTf(routeSetV4Name, "%s", err)
				},
			},
		)
	}()

	return nil
}

var _ routesAdmin.SetProviderV6WithCtx = (*routesAdminSetProviderV6Impl)(nil)

type routesAdminSetProviderV6Impl struct {
	ns *Netstack
}

func (impl *routesAdminSetProviderV6Impl) NewRouteSet(ctx_ fidl.Context, request routesAdmin.RouteSetV6WithCtxInterfaceRequest) error {
	ctx, cancel := context.WithCancel(context.Background())

	go func() {
		defer cancel()

		setImpl := routeSetV6Impl{
			routeSet: makeRouteSet[fuchsianet.Ipv6Address](impl.ns),
		}
		defer setImpl.routeSet.close()

		component.Serve(
			ctx,
			&routesAdmin.RouteSetV6WithCtxStub{Impl: &setImpl},
			request.Channel,
			component.ServeOptions{
				Concurrent: true,
				OnError: func(err error) {
					_ = syslog.ErrorTf(routeSetV6Name, "%s", err)
				},
			},
		)
	}()

	return nil
}

type routeSetV4Impl struct {
	routeSet routeSet[fuchsianet.Ipv4Address]
}

var _ routesAdmin.RouteSetV4WithCtx = (*routeSetV4Impl)(nil)

func (r *routeSetV4Impl) AddRoute(ctx_ fidl.Context, fidlRoute fnetRoutes.RouteV4) (routesAdmin.RouteSetV4AddRouteResult, error) {
	var result routesAdmin.RouteSetV4AddRouteResult
	route, validationResult := fidlconv.FromFidlRouteV4(fidlRoute)

	switch validationResult {
	case fidlconv.RouteOk:
		didAdd, err := r.routeSet.addRoute(route)
		if err != nil {
			if errors.Is(err, routes.ErrNoSuchNIC) {
				result.SetErr(routesAdmin.RouteSetErrorPreviouslyAuthenticatedInterfaceNoLongerExists)
				return result, nil
			} else {
				_ = syslog.ErrorTf(routeSetV4Name, "error while adding route: %s", err)
			}
		}
		result.SetResponse(routesAdmin.RouteSetV4AddRouteResponse{
			DidAdd: didAdd,
		})
	case fidlconv.RouteInvalidDestinationSubnet:
		result.SetErr(routesAdmin.RouteSetErrorInvalidDestinationSubnet)
	case fidlconv.RouteInvalidNextHop:
		result.SetErr(routesAdmin.RouteSetErrorInvalidNextHop)
	case fidlconv.RouteInvalidUnknownAction:
		result.SetErr(routesAdmin.RouteSetErrorUnsupportedAction)
	default:
		panic(fmt.Sprintf("unrecognized RouteValidationResult: %d", validationResult))
	}

	return result, nil
}

func (*routeSetV4Impl) AuthenticateForInterface(ctx_ fidl.Context, credential admin.ProofOfInterfaceAuthorization) (routesAdmin.RouteSetV4AuthenticateForInterfaceResult, error) {
	_ = syslog.WarnTf(routeSetV4Name, "TODO(https://fxbug.dev/129218): implement interface authentication")
	var result routesAdmin.RouteSetV4AuthenticateForInterfaceResult
	result.SetResponse(routesAdmin.RouteSetV4AuthenticateForInterfaceResponse{})
	return result, nil
}

func (r *routeSetV4Impl) RemoveRoute(ctx_ fidl.Context, fidlRoute fnetRoutes.RouteV4) (routesAdmin.RouteSetV4RemoveRouteResult, error) {
	var result routesAdmin.RouteSetV4RemoveRouteResult
	route, validationResult := fidlconv.FromFidlRouteV4(fidlRoute)

	switch validationResult {
	case fidlconv.RouteOk:
		didRemove, err := r.routeSet.removeRoute(route)
		if err != nil {
			if errors.Is(err, routes.ErrNoSuchNIC) {
				result.SetErr(routesAdmin.RouteSetErrorPreviouslyAuthenticatedInterfaceNoLongerExists)
				return result, nil
			} else {
				_ = syslog.ErrorTf(routeSetV4Name, "error while removing route: %s", err)
			}
		}
		result.SetResponse(routesAdmin.RouteSetV4RemoveRouteResponse{
			DidRemove: didRemove,
		})
	case fidlconv.RouteInvalidDestinationSubnet:
		result.SetErr(routesAdmin.RouteSetErrorInvalidDestinationSubnet)
	case fidlconv.RouteInvalidNextHop:
		result.SetErr(routesAdmin.RouteSetErrorInvalidNextHop)
	case fidlconv.RouteInvalidUnknownAction:
		result.SetErr(routesAdmin.RouteSetErrorUnsupportedAction)
	default:
		panic(fmt.Sprintf("unrecognized RouteValidationResult: %d", validationResult))
	}

	return result, nil
}

type routeSetV6Impl struct {
	routeSet routeSet[fuchsianet.Ipv6Address]
}

var _ routesAdmin.RouteSetV6WithCtx = (*routeSetV6Impl)(nil)

func (r *routeSetV6Impl) AddRoute(ctx_ fidl.Context, fidlRoute fnetRoutes.RouteV6) (routesAdmin.RouteSetV6AddRouteResult, error) {
	var result routesAdmin.RouteSetV6AddRouteResult
	route, validationResult := fidlconv.FromFidlRouteV6(fidlRoute)

	switch validationResult {
	case fidlconv.RouteOk:
		didAdd, err := r.routeSet.addRoute(route)
		if err != nil {
			if errors.Is(err, routes.ErrNoSuchNIC) {
				result.SetErr(routesAdmin.RouteSetErrorUnauthenticated)
				return result, nil
			} else {
				_ = syslog.ErrorTf(routeSetV6Name, "error while adding route: %s", err)
			}
		}
		result.SetResponse(routesAdmin.RouteSetV6AddRouteResponse{
			DidAdd: didAdd,
		})

	case fidlconv.RouteInvalidDestinationSubnet:
		result.SetErr(routesAdmin.RouteSetErrorInvalidDestinationSubnet)
	case fidlconv.RouteInvalidNextHop:
		_ = syslog.ErrorTf(routeSetV6Name, "rejecting route %#v due to invalid next hop", route)
		result.SetErr(routesAdmin.RouteSetErrorInvalidNextHop)
	case fidlconv.RouteInvalidUnknownAction:
		result.SetErr(routesAdmin.RouteSetErrorUnsupportedAction)
	default:
		panic(fmt.Sprintf("unrecognized RouteValidationResult: %d", validationResult))
	}

	return result, nil
}

func (*routeSetV6Impl) AuthenticateForInterface(ctx_ fidl.Context, credential admin.ProofOfInterfaceAuthorization) (routesAdmin.RouteSetV6AuthenticateForInterfaceResult, error) {
	_ = syslog.WarnTf(routeSetV6Name, "TODO(https://fxbug.dev/129218): implement interface authentication")
	var result routesAdmin.RouteSetV6AuthenticateForInterfaceResult
	result.SetResponse(routesAdmin.RouteSetV6AuthenticateForInterfaceResponse{})
	return result, nil
}

func (r *routeSetV6Impl) RemoveRoute(ctx_ fidl.Context, fidlRoute fnetRoutes.RouteV6) (routesAdmin.RouteSetV6RemoveRouteResult, error) {
	var result routesAdmin.RouteSetV6RemoveRouteResult
	route, validationResult := fidlconv.FromFidlRouteV6(fidlRoute)

	switch validationResult {
	case fidlconv.RouteOk:
		didRemove, err := r.routeSet.removeRoute(route)
		if err != nil {
			if errors.Is(err, routes.ErrNoSuchNIC) {
				result.SetErr(routesAdmin.RouteSetErrorUnauthenticated)
				return result, nil
			} else {
				_ = syslog.ErrorTf(routeSetV6Name, "error while removing route: %s", err)
			}
		}
		result.SetResponse(routesAdmin.RouteSetV6RemoveRouteResponse{
			DidRemove: didRemove,
		})
	case fidlconv.RouteInvalidDestinationSubnet:
		result.SetErr(routesAdmin.RouteSetErrorInvalidDestinationSubnet)
	case fidlconv.RouteInvalidNextHop:
		result.SetErr(routesAdmin.RouteSetErrorInvalidNextHop)
	case fidlconv.RouteInvalidUnknownAction:
		result.SetErr(routesAdmin.RouteSetErrorUnsupportedAction)
	default:
		panic(fmt.Sprintf("unrecognized RouteValidationResult: %d", validationResult))
	}

	return result, nil
}
