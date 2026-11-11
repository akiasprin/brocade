// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
// Package networkattachments defines the versioned node-local Cubelet contract.
package networkattachments

import "encoding/json"

const Contract = "cubevs-attachments-v2"
const ActionKey = "cube.network.attachments.action"
const PayloadKey = "cube.network.attachments.request"
const ResultKey = "cube.network.attachments.result"
const MaxMessage = 128 * 1024

type Source struct {
	Bridge      string `json:"bridge"`
	IPv4Pool    string `json:"ipv4_pool"`
	IPv4Gateway string `json:"ipv4_gateway"`
	IPv6Pool    string `json:"ipv6_pool,omitempty"`
	IPv6Gateway string `json:"ipv6_gateway,omitempty"`
}
type Attachment struct {
	GuardIntent       bool          `json:"guard_intent,omitempty"`
	Allocated         *AllocatedNIC `json:"allocated,omitempty"`
	PreviousIPv4      string        `json:"previous_ipv4,omitempty"`
	PreviousIPv6      string        `json:"previous_ipv6,omitempty"`
	Ifindex           int           `json:"ifindex"`
	Slot              int           `json:"slot"`
	SourceSHA256      string        `json:"source_sha256"`
	ID                string        `json:"id"`
	Source            string        `json:"source"`
	Tap               string        `json:"tap"`
	MAC               string        `json:"mac"`
	DeviceID          string        `json:"device_id"`
	IPv4              string        `json:"ipv4"`
	IPv6              string        `json:"ipv6,omitempty"`
	Removed           bool          `json:"removed"`
	RemovedGeneration uint64        `json:"removed_generation,omitempty"`
}

// Interface is a stable attachment to one node-authorized source.
type Interface struct {
	Allocated   *AllocatedNIC `json:"allocated,omitempty"`
	ID          string        `json:"id"`
	SourceID    string        `json:"source_id"`
	Purpose     string        `json:"purpose"`
	DefaultIPv4 bool          `json:"default_ipv4"`
	DefaultIPv6 bool          `json:"default_ipv6"`
}

type Request struct {
	Routing json.RawMessage `json:"routing"`
	// Detach lists Console-assigned cleanup identities absent from Interfaces.
	// A stopped release confirms these only after physical network absence.
	Detach     []string    `json:"detach,omitempty"`
	Contract   string      `json:"contract"`
	InstanceID string      `json:"instance_id"`
	RuntimeID  string      `json:"runtime_id"`
	Generation uint64      `json:"generation"`
	Interfaces []Interface `json:"interfaces"`
	Release    bool        `json:"release"`
	Forget     bool        `json:"forget"`
}
type Observation struct {
	ID        string   `json:"id"`
	Device    string   `json:"device"`
	MAC       string   `json:"mac"`
	Addresses []string `json:"addresses"`
	Attached  bool     `json:"attached"`
}
type PolicyObservation struct {
	RoutingRevision         uint64  `json:"routing_revision"`
	ContactRoutePreserved   bool    `json:"contact_route_preserved"`
	BusinessFallbackBlocked bool    `json:"business_fallback_blocked"`
	DefaultIPv4Interface    *string `json:"default_ipv4_interface"`
	DefaultIPv6Interface    *string `json:"default_ipv6_interface"`
}
type Result struct {
	Contract          string             `json:"contract"`
	Interfaces        []Observation      `json:"interfaces"`
	Network           *PolicyObservation `json:"network"`
	Detached          []string           `json:"detached_interfaces"`
	NetworkAbsent     bool               `json:"network_absent"`
	ActualVCPUs       uint64             `json:"actual_vcpus"`
	ActualMemoryBytes uint64             `json:"actual_memory_bytes"`
	ShimVersion       string             `json:"shim_version,omitempty"`
	RestartRequired   bool               `json:"restart_required,omitempty"`
}
type Catalogue struct {
	NetworkQuota      bool              `json:"network_quota"`
	ManagedNICs       bool              `json:"managed_nics"`
	PrimaryIPDelivery bool              `json:"primary_ip_delivery,omitempty"`
	RouteServices     bool              `json:"route_services,omitempty"` // port-selected egress as Guest policy rules
	Contract          string            `json:"contract"`
	MaxInterfaces     uint16            `json:"max_interfaces"`
	Sources           map[string]Source `json:"sources"`
	ShimVersion       string            `json:"shim_version,omitempty"`
}

// AllocatedNIC is a Store assignment, never a request for local address selection.
type AllocatedNIC struct {
	ResourceDomain string `json:"resource_domain"`
	AllocationID   string `json:"allocation_id"`
	PublicationID  string `json:"publication_id"`
	MAC            string `json:"mac"`
	IPv4           string `json:"ipv4"`
	IPv6           string `json:"ipv6"`
	MTU            uint16 `json:"mtu"`
	Blocked        bool   `json:"blocked"`
	QuotaExceeded  bool   `json:"quota_exceeded"`
}
