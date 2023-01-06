// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package license

type LicenseConfig struct {
	// FuchsiaDir is the path to the root of your fuchsia workspace.
	// Typically ~/fuchsia, but can be set by environment variables
	// or command-line arguments.
	FuchsiaDir string `json:"fuchsiaDir"`

	// List of PatternRoot objects, defining paths to folders containing
	// license patterns.
	PatternRoots []*PatternRoot `json:"patternRoot"`

	// Allowlists define projects that have been given approval to use
	// a given restricted license type.
	Allowlists               []*Allowlist `json:"allowlists"`
	AllowlistsSoftTransition []*Allowlist `json:"exceptions"`
}

type PatternRoot struct {
	// Path to the directory holding the license patterns.
	Paths []string `json:"paths"`
	// Optional freeform text field for describing how this directory is
	// meant to be used.
	Notes []string `json:"notes"`
}

// Projects must only contain approved license types.
//
// If they have any license texts that are not approved, an exception must
// exist to allow that project access to that license type.
//
// This struct describes how the allowlist is formatted.
// TODO(fxbug.dev/109828): Rename "Exception" to "Allowlist".
type Allowlist struct {
	// LicenseType describes the type of license that this pattern matches
	// with (e.g. bsd-3).
	LicenseType string `json:"licenseType"`

	// PatternRoot is a reference field, pointing to the license pattern
	// that matches this license text.
	PatternRoot string `json:"patternRoot"`

	// List of allowlist entries.
	Entries []*AllowlistEntry `json:"entries"`

	// Example is a freeform text field used to make verification easier.
	// Provide a string snippet or a URL to license text that requires
	// this allowlist entry.
	Example string `json:"example"`

	// Notes is a freeform text field which can be used to explain
	// why these project were granted this exception.
	Notes []string `json:"notes"`
}

// Each allowlist entry can define a bug which should describe why / when the project
// was allowlisted.
//
// In the future, bug entries will be required for each exception entry.
type AllowlistEntry struct {
	// Link to a bug granting this exception.
	// TODO(b/264579404): Make this a required field
	Bug string `json:"bug"`

	// Project paths that this exception applies to.
	Projects []string `json:"projects"`
}

var Config *LicenseConfig

func init() {
	Config = NewConfig()
}

func NewConfig() *LicenseConfig {
	return &LicenseConfig{
		PatternRoots: make([]*PatternRoot, 0),
		Allowlists:   make([]*Allowlist, 0),
	}
}

func (c *LicenseConfig) Merge(other *LicenseConfig) {
	if c.FuchsiaDir == "" {
		c.FuchsiaDir = other.FuchsiaDir
	}

	if c.PatternRoots == nil {
		c.PatternRoots = make([]*PatternRoot, 0)
	}
	if other.PatternRoots == nil {
		other.PatternRoots = make([]*PatternRoot, 0)
	}
	c.PatternRoots = append(c.PatternRoots, other.PatternRoots...)

	if c.Allowlists == nil {
		c.Allowlists = make([]*Allowlist, 0)
	}
	if c.AllowlistsSoftTransition == nil {
		c.AllowlistsSoftTransition = make([]*Allowlist, 0)
	}
	if other.Allowlists == nil {
		other.Allowlists = make([]*Allowlist, 0)
	}
	if other.AllowlistsSoftTransition == nil {
		other.AllowlistsSoftTransition = make([]*Allowlist, 0)
	}
	c.Allowlists = append(c.Allowlists, other.Allowlists...)
	c.Allowlists = append(c.Allowlists, c.AllowlistsSoftTransition...)
	c.Allowlists = append(c.Allowlists, other.AllowlistsSoftTransition...)
}
