// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package flasher

import (
	"context"
	"io"
	"os"
	"os/exec"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"golang.org/x/crypto/ssh"

	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type FfxFlasher struct {
	Ffx              *ffx.FFXTool
	FlashManifest    string
	useProductBundle bool
	sshPublicKey     ssh.PublicKey
	target           string
}

type ScriptFlasher struct {
	FlashScript   string
	FlashManifest string
	sshPublicKey  ssh.PublicKey
	target        string
	stdout        io.Writer
}

type Flasher interface {
	Flash(ctx context.Context) error
	SetTarget(ctx context.Context, target string) error
	Close() error
}

// NewFfxFlasher constructs a new flasher that uses `ffx` as the FFXTool used
// to flash a device using flash.json located at `flashManifest`. Also accepts a
// number of optional parameters.
func NewFfxFlasher(ffx *ffx.FFXTool, flashManifest string, useProductBundle bool, options ...FfxFlasherOption) (*FfxFlasher, error) {
	p := &FfxFlasher{
		Ffx:              ffx,
		FlashManifest:    flashManifest,
		useProductBundle: useProductBundle,
	}

	for _, opt := range options {
		if err := opt(p); err != nil {
			return nil, err
		}
	}

	return p, nil
}

func NewScriptFlasher(flashScript string, flashManifest string, options ...ScriptFlasherOption) (*ScriptFlasher, error) {
	p := &ScriptFlasher{
		FlashScript:   flashScript,
		FlashManifest: flashManifest,
	}

	for _, opt := range options {
		if err := opt(p); err != nil {
			return nil, err
		}
	}

	return p, nil
}

type FfxFlasherOption func(p *FfxFlasher) error

type ScriptFlasherOption func(p *ScriptFlasher) error

// Sets the SSH public key that the Flasher will bake into the device as an
// authorized key.
func SSHPublicKey(publicKey ssh.PublicKey) FfxFlasherOption {
	return func(p *FfxFlasher) error {
		p.sshPublicKey = publicKey
		return nil
	}
}

// Sets the SSH public key that the Flasher will bake into the device as an
// authorized key.
func ScriptFlasherSSHPublicKey(publicKey ssh.PublicKey) ScriptFlasherOption {
	return func(p *ScriptFlasher) error {
		p.sshPublicKey = publicKey
		return nil
	}
}

// Send stdout from the ffx target flash scripts to `writer`. Defaults to the parent
// stdout.
func Stdout(writer io.Writer) FfxFlasherOption {
	return func(p *FfxFlasher) error {
		p.Ffx.SetStdout(writer)
		return nil
	}
}

// Send stdout flash scripts to `writer`. Defaults to the parent
// stdout.
func ScriptFlasherStdout(writer io.Writer) ScriptFlasherOption {
	return func(p *ScriptFlasher) error {
		p.stdout = writer
		return nil
	}
}

// Close cleans up the resources associated with the flasher.
func (p *FfxFlasher) Close() error {
	return p.Ffx.Close()
}

func (p *ScriptFlasher) Close() error {
	return nil
}

// SetTarget sets the target to flash.
func (p *FfxFlasher) SetTarget(ctx context.Context, target string) error {
	p.target = target
	return p.Ffx.TargetAdd(ctx, target)
}

func (p *ScriptFlasher) SetTarget(ctx context.Context, target string) error {
	p.target = target
	return nil
}

// Flash a device with flash.json manifest.
func (p *FfxFlasher) Flash(ctx context.Context) error {
	flasherArgs := []string{}

	// Write out the public key's authorized keys.
	if p.sshPublicKey != nil {
		authorizedKeys, err := os.CreateTemp("", "")
		if err != nil {
			return err
		}
		defer os.Remove(authorizedKeys.Name())

		if _, err := authorizedKeys.Write(ssh.MarshalAuthorizedKey(p.sshPublicKey)); err != nil {
			return err
		}

		if err := authorizedKeys.Close(); err != nil {
			return err
		}

		flasherArgs = append(flasherArgs, "--authorized-keys", authorizedKeys.Name())
	}
	if p.useProductBundle {
		flasherArgs = append(flasherArgs, "--product-bundle")
	}
	return p.Ffx.Flash(ctx, p.target, p.FlashManifest, flasherArgs...)
}

func (p *ScriptFlasher) Flash(ctx context.Context) error {
	flasherArgs := []string{}

	// Write out the public key's authorized keys.
	if p.sshPublicKey != nil {
		authorizedKeys, err := os.CreateTemp("", "")
		if err != nil {
			return err
		}
		defer os.Remove(authorizedKeys.Name())

		if _, err := authorizedKeys.Write(ssh.MarshalAuthorizedKey(p.sshPublicKey)); err != nil {
			return err
		}

		if err := authorizedKeys.Close(); err != nil {
			return err
		}

		flasherArgs = append(flasherArgs, "--ssh-key", authorizedKeys.Name())
	}

	return p.runScript(ctx, flasherArgs)

}

func (p *ScriptFlasher) runScript(ctx context.Context, args []string) error {
	path, err := exec.LookPath(p.FlashScript)
	if err != nil {
		return err
	}

	logger.Infof(ctx, "running: %s %q", path, args)
	cmd := exec.CommandContext(ctx, path, args...)
	if p.stdout != nil {
		cmd.Stdout = p.stdout
	} else {
		cmd.Stdout = os.Stdout
	}
	cmd.Stderr = os.Stderr

	return cmd.Run()
}
