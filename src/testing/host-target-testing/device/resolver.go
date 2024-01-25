// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package device

import (
	"context"
	"fmt"
	"time"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type DeviceResolver interface {
	// NodeNames returns a list of nodenames for a device.
	NodeNames() []string

	// Resolve the device's nodename into a hostname.
	ResolveName(ctx context.Context) (string, error)

	// Block until the device appears to be in netboot.
	WaitToFindDeviceInNetboot(ctx context.Context) (string, error)
}

// ConstnatAddressResolver returns a fixed hostname for the specified nodename.
type ConstantHostResolver struct {
	nodeName    string
	oldNodeName string
	host        string
}

// NewConstantAddressResolver constructs a fixed host.
func NewConstantHostResolver(ctx context.Context, nodeName string, host string) ConstantHostResolver {
	oldNodeName, err := translateToOldNodeName(nodeName)
	if err == nil {
		logger.Infof(ctx, "translated device name %q to old nodename %q", nodeName, oldNodeName)
	} else {
		logger.Warningf(ctx, "could not translate %q into old device name: %v", nodeName, err)
		oldNodeName = ""
	}

	return ConstantHostResolver{
		nodeName:    nodeName,
		oldNodeName: oldNodeName,
		host:        host,
	}
}

func (r ConstantHostResolver) NodeNames() []string {
	if r.oldNodeName == "" {
		return []string{r.nodeName}
	} else {
		return []string{r.nodeName, r.oldNodeName}
	}
}

func (r ConstantHostResolver) ResolveName(ctx context.Context) (string, error) {
	return r.host, nil
}

func (r ConstantHostResolver) WaitToFindDeviceInNetboot(ctx context.Context) (string, error) {
	// We have no way to tell if the device is in netboot, so just exit.
	logger.Warningf(ctx, "ConstantHostResolver cannot tell if device is in netboot, assuming nodename is %s", r.nodeName)
	return r.nodeName, nil
}

// DeviceFinderResolver uses `device-finder` to resolve a nodename into a hostname.
// The logic here should match get-fuchsia-device-addr (//tools/devshell/lib/vars.sh).
type DeviceFinderResolver struct {
	deviceFinder *DeviceFinder
	nodeName     string
	oldNodeName  string
}

// NewDeviceFinderResolver constructs a new `DeviceFinderResolver` for the specific
func NewDeviceFinderResolver(ctx context.Context, deviceFinder *DeviceFinder, nodeName string) (*DeviceFinderResolver, error) {
	if nodeName == "" {
		entries, err := deviceFinder.List(ctx, Mdns)
		if err != nil {
			return nil, fmt.Errorf("failed to list devices: %w", err)
		}
		if len(entries) == 0 {
			return nil, fmt.Errorf("no devices found")
		}

		if len(entries) != 1 {
			return nil, fmt.Errorf("cannot use empty nodename with multiple devices: %v", entries)
		}

		nodeName = entries[0].NodeName
	}

	var oldNodeName string

	// FIXME(https://fxbug.dev/42150466) we can switch back to the resolver
	// resolving a single name after we no longer support testing builds
	// older than 2021-02-22.
	if name, err := translateToOldNodeName(nodeName); err == nil {
		logger.Infof(ctx, "translated device name %q to old nodename %q", nodeName, name)
		oldNodeName = name
	} else {
		logger.Warningf(ctx, "could not translate %q into old device name: %v", nodeName, err)
	}

	return &DeviceFinderResolver{
		deviceFinder: deviceFinder,
		nodeName:     nodeName,
		oldNodeName:  oldNodeName,
	}, nil
}

func (r *DeviceFinderResolver) NodeNames() []string {
	return []string{r.nodeName, r.oldNodeName}
}

func (r *DeviceFinderResolver) ResolveName(ctx context.Context) (string, error) {
	entry, err := r.deviceFinder.Resolve(ctx, Mdns, r.NodeNames())
	if err != nil {
		return "", err
	}

	return entry.Address, nil
}

func (r *DeviceFinderResolver) WaitToFindDeviceInNetboot(ctx context.Context) (string, error) {
	nodeNames := r.NodeNames()

	// Wait for the device to be listening in netboot.
	logger.Infof(ctx, "waiting for the to be listening on the nodenames: %v", nodeNames)

	attempt := 0
	for {
		attempt += 1

		if entry, err := r.deviceFinder.Resolve(ctx, Netboot, nodeNames); err == nil {
			logger.Infof(ctx, "device %v is listening on %q", nodeNames, entry)
			return entry.NodeName, nil
		} else {
			logger.Infof(ctx, "attempt %d failed to resolve nodenames %v: %v", attempt, nodeNames, err)
		}
	}
}

// FfxResolver uses `ffx target list` to resolve a nodename into a hostname.
type FfxResolver struct {
	ffx         *ffx.FFXTool
	nodeName    string
	oldNodeName string
}

// NewFffResolver constructs a new `FfxResolver` for the specific nodename.
func NewFfxResolver(ctx context.Context, ffx *ffx.FFXTool, nodeName string) (*FfxResolver, error) {
	if nodeName == "" {
		entries, err := ffx.TargetList(ctx)

		if err != nil {
			return nil, fmt.Errorf("failed to list devices: %w", err)
		}
		if len(entries) == 0 {
			return nil, fmt.Errorf("no devices found")
		}

		if len(entries) != 1 {
			return nil, fmt.Errorf("cannot use empty nodename with multiple devices: %v", entries)
		}

		nodeName = entries[0].NodeName
	}

	var oldNodeName string

	// FIXME(https://fxbug.dev/42150466) we can switch back to the resolver
	// resolving a single name after we no longer support testing builds
	// older than 2021-02-22.
	if name, err := translateToOldNodeName(nodeName); err == nil {
		logger.Infof(ctx, "translated device name %q to old nodename %q", nodeName, name)
		oldNodeName = name
	} else {
		logger.Warningf(ctx, "could not translate %q into old device name: %v", nodeName, err)
	}

	return &FfxResolver{
		ffx:         ffx,
		nodeName:    nodeName,
		oldNodeName: oldNodeName,
	}, nil
}

func (r *FfxResolver) NodeNames() []string {
	return []string{r.nodeName, r.oldNodeName}
}

func (r *FfxResolver) ResolveName(ctx context.Context) (string, error) {
	nodeNames := r.NodeNames()
	logger.Infof(ctx, "resolving the nodenames %v", nodeNames)

	targets, err := r.ffx.TargetListForNode(ctx, nodeNames)
	if err != nil {
		return "", err
	}

	logger.Infof(ctx, "resolved the nodenames %v to %v", nodeNames, targets)

	if len(targets) == 0 {
		return "", fmt.Errorf("no addresses found for nodenames: %v", nodeNames)
	}

	if len(targets) > 1 {
		return "", fmt.Errorf("multiple addresses found for nodenames %v: %v", nodeNames, targets)
	}

	return targets[0].Addresses[0], nil
}

func (r *FfxResolver) WaitToFindDeviceInNetboot(ctx context.Context) (string, error) {
	// Exit early if ffx is not configured to listen for devices in zedboot.
	supportsZedbootDiscovery, err := r.ffx.SupportsZedbootDiscovery(ctx)
	if err != nil {
		return "", err
	}

	if !supportsZedbootDiscovery {
		logger.Warningf(ctx, "ffx not configured to listen for devices in zedboot, assuming nodename is %s", r.nodeName)
		return r.nodeName, nil
	}

	nodeNames := r.NodeNames()

	// Wait for the device to be listening in netboot.
	logger.Infof(ctx, "waiting for the to be listening on the nodenames: %v", nodeNames)

	attempt := 0
	for {
		attempt += 1

		if entries, err := r.ffx.TargetListForNode(ctx, nodeNames); err == nil {
			for _, entry := range entries {
				logger.Infof(ctx, "device is in %v", entry.TargetState)
				if entry.TargetState == "Zedboot (R)" {
					logger.Infof(ctx, "device %v is listening on %q", entry.NodeName, entry)
					return entry.NodeName, nil
				}
			}

			logger.Infof(ctx, "attempt %d waiting for device to boot into zedboot", attempt)
			time.Sleep(5 * time.Second)
		} else {
			logger.Infof(ctx, "attempt %d failed to resolve nodenames %v: %v", attempt, nodeNames, err)
		}

	}
}
