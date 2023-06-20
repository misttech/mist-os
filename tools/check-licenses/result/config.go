// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package result

type ResultConfig struct {
	FuchsiaDir        string      `json:"fuchsiaDir"`
	Target            string      `json:"target"`
	SPDXDocName       string      `json:"spdxDocName"`
	OutDir            string      `json:"outDir"`
	RootOutDir        string      `json:"rootOutDir"`
	LicenseOutDir     string      `json:"licenseOutDir"`
	Outputs           []string    `json:"outputs"`
	Templates         []*Template `json:"templates"`
	Zip               bool        `json:"zip"`
	GnGenOutputFile   string      `json:"gnGenOutputFile"`
	OutputLicenseFile bool        `json:"outputLicenseFile"`
	RunAnalysis       bool        `json:"runAnalysis"`
	BuildInfoVersion  string      `json:"buildInfoVersion"`
	BuildInfoProduct  string      `json:"buildInfoProduct"`
	BuildInfoBoard    string      `json:"buildInfoBoard"`

	Checks    []*Check `json:"checks"`
	CheckURLs bool

	AllowLists []*AllowList `json::allowlists"`
}

type Template struct {
	Paths []string `json:"paths"`
	Notes []string `json:"notes"`
}

type AllowList struct {
	Name      string            `json:"name"`
	MatchType string            `json:"matchType"`
	Entries   []*AllowListEntry `json:"entries"`
}

type AllowListEntry struct {
	Projects []string `json:"projects"`
	Bug      string   `json:"bug"`
	Notes    []string `json:"notes"`
}

var Config *ResultConfig

func NewConfig() *ResultConfig {
	return &ResultConfig{
		Outputs:           make([]string, 0),
		Templates:         make([]*Template, 0),
		Checks:            make([]*Check, 0),
		AllowLists:        make([]*AllowList, 0),
		OutputLicenseFile: false,
		RunAnalysis:       false,
	}
}

func (c *ResultConfig) Merge(other *ResultConfig) {
	if c.FuchsiaDir == "" {
		c.FuchsiaDir = other.FuchsiaDir
	}
	if c.Target == "" {
		c.Target = other.Target
	}
	if c.RootOutDir == "" {
		c.RootOutDir = other.RootOutDir
	}
	if c.OutDir == "" {
		c.OutDir = other.OutDir
	}
	if c.LicenseOutDir == "" {
		c.LicenseOutDir = other.LicenseOutDir
	}
	c.Templates = append(c.Templates, other.Templates...)
	c.Outputs = append(c.Outputs, other.Outputs...)
	c.Zip = c.Zip || other.Zip
	if c.GnGenOutputFile == "" {
		c.GnGenOutputFile = other.GnGenOutputFile
	}
	c.OutputLicenseFile = c.OutputLicenseFile || other.OutputLicenseFile
	c.RunAnalysis = c.RunAnalysis || other.RunAnalysis
	if c.BuildInfoVersion == "" {
		c.BuildInfoVersion = other.BuildInfoVersion
	}
	if c.BuildInfoProduct == "" {
		c.BuildInfoProduct = other.BuildInfoProduct
	}
	if c.BuildInfoBoard == "" {
		c.BuildInfoBoard = other.BuildInfoBoard
	}
	if c.SPDXDocName == "" {
		c.SPDXDocName = other.SPDXDocName
	}
	c.Checks = append(c.Checks, other.Checks...)
	c.CheckURLs = c.CheckURLs || other.CheckURLs

	c.AllowLists = append(c.AllowLists, other.AllowLists...)
}
