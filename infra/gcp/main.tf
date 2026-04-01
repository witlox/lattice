# Lattice GCP Test Infrastructure
#
# Spins up a minimal lattice cluster for integration testing:
# - 3 quorum nodes (Raft cluster + API)
# - 2 compute nodes (node agents)
# - 1 registry (OCI + ORAS for uenv images)
# - 1 VictoriaMetrics (telemetry)
# - Shared storage via GCS (S3-compatible)
#
# Usage:
#   terraform init
#   terraform apply -var="project_id=my-project" -var="ssh_public_key_file=~/.ssh/id_ed25519.pub"
#   # ... run tests ...
#   terraform destroy  # manual cleanup

terraform {
  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 5.0"
    }
  }
}

provider "google" {
  project = var.project_id
  region  = var.region
  zone    = var.zone
}

# ─── Variables ────────────────────────────────────────────────

variable "project_id" {
  description = "GCP project ID"
  type        = string
}

variable "region" {
  description = "GCP region"
  type        = string
  default     = "europe-west1"
}

variable "zone" {
  description = "GCP zone"
  type        = string
  default     = "europe-west1-b"
}

variable "ssh_public_key_file" {
  description = "Path to SSH public key for VM access"
  type        = string
  default     = "~/.ssh/id_ed25519.pub"
}

variable "machine_type" {
  description = "VM machine type (CPU-only, no GPUs)"
  type        = string
  default     = "e2-standard-2"
}

variable "lattice_version" {
  description = "Lattice release version to deploy (or 'latest')"
  type        = string
  default     = "latest"
}

locals {
  ssh_key = file(pathexpand(var.ssh_public_key_file))
  prefix  = "lattice-test"
}

# ─── Network ──────────────────────────────────────────────────

resource "google_compute_network" "lattice" {
  name                    = "${local.prefix}-network"
  auto_create_subnetworks = false
}

resource "google_compute_subnetwork" "lattice" {
  name          = "${local.prefix}-subnet"
  ip_cidr_range = "10.0.0.0/24"
  network       = google_compute_network.lattice.id
}

resource "google_compute_firewall" "internal" {
  name    = "${local.prefix}-internal"
  network = google_compute_network.lattice.id

  allow {
    protocol = "tcp"
    ports    = ["0-65535"]
  }
  allow {
    protocol = "udp"
    ports    = ["0-65535"]
  }
  allow {
    protocol = "icmp"
  }

  source_ranges = ["10.0.0.0/24"]
}

resource "google_compute_firewall" "ssh" {
  name    = "${local.prefix}-ssh"
  network = google_compute_network.lattice.id

  allow {
    protocol = "tcp"
    ports    = ["22"]
  }

  source_ranges = ["0.0.0.0/0"]
}

resource "google_compute_firewall" "api" {
  name    = "${local.prefix}-api"
  network = google_compute_network.lattice.id

  allow {
    protocol = "tcp"
    ports    = ["8080", "50051"]  # REST + gRPC
  }

  source_ranges = ["0.0.0.0/0"]
  target_tags   = ["lattice-quorum"]
}

# ─── Quorum Nodes (3x) ──────────────────────────────────────

resource "google_compute_instance" "quorum" {
  count        = 3
  name         = "${local.prefix}-quorum-${count.index + 1}"
  machine_type = var.machine_type
  tags         = ["lattice-quorum"]

  boot_disk {
    initialize_params {
      image = "ubuntu-os-cloud/ubuntu-2404-lts-amd64"
      size  = 50
    }
  }

  network_interface {
    subnetwork = google_compute_subnetwork.lattice.id
    access_config {} # External IP for SSH
  }

  metadata = {
    ssh-keys = "lattice:${local.ssh_key}"
  }

  metadata_startup_script = templatefile("${path.module}/scripts/setup-quorum.sh", {
    node_id         = count.index + 1
    node_count      = 3
    peer_ips        = join(",", [for i in range(3) : "10.0.0.${10 + i}"])
    lattice_version = var.lattice_version
  })

  labels = {
    role = "quorum"
    node = tostring(count.index + 1)
  }
}

# ─── Compute Nodes (2x) ──────────────────────────────────────

resource "google_compute_instance" "compute" {
  count        = 2
  name         = "${local.prefix}-compute-${count.index + 1}"
  machine_type = var.machine_type

  boot_disk {
    initialize_params {
      image = "ubuntu-os-cloud/ubuntu-2404-lts-amd64"
      size  = 50
    }
  }

  network_interface {
    subnetwork = google_compute_subnetwork.lattice.id
    access_config {}
  }

  metadata = {
    ssh-keys = "lattice:${local.ssh_key}"
  }

  metadata_startup_script = templatefile("${path.module}/scripts/setup-compute.sh", {
    quorum_ip       = google_compute_instance.quorum[0].network_interface[0].network_ip
    node_index      = count.index
    lattice_version = var.lattice_version
  })

  labels = {
    role = "compute"
    node = tostring(count.index + 1)
  }

  depends_on = [google_compute_instance.quorum]
}

# ─── Registry ────────────────────────────────────────────────

resource "google_compute_instance" "registry" {
  name         = "${local.prefix}-registry"
  machine_type = "e2-small"

  boot_disk {
    initialize_params {
      image = "ubuntu-os-cloud/ubuntu-2404-lts-amd64"
      size  = 50
    }
  }

  network_interface {
    subnetwork = google_compute_subnetwork.lattice.id
    access_config {}
  }

  metadata = {
    ssh-keys = "lattice:${local.ssh_key}"
  }

  metadata_startup_script = templatefile("${path.module}/scripts/setup-registry.sh", {})

  labels = {
    role = "registry"
  }
}

# ─── VictoriaMetrics ─────────────────────────────────────────

resource "google_compute_instance" "victoriametrics" {
  name         = "${local.prefix}-tsdb"
  machine_type = "e2-small"

  boot_disk {
    initialize_params {
      image = "ubuntu-os-cloud/ubuntu-2404-lts-amd64"
      size  = 20
    }
  }

  network_interface {
    subnetwork = google_compute_subnetwork.lattice.id
    access_config {}
  }

  metadata = {
    ssh-keys = "lattice:${local.ssh_key}"
  }

  metadata_startup_script = <<-EOF
    #!/bin/bash
    set -e
    apt-get update -qq && apt-get install -y -qq wget
    wget -qO /usr/local/bin/victoria-metrics https://github.com/VictoriaMetrics/VictoriaMetrics/releases/download/v1.101.0/victoria-metrics-linux-amd64-v1.101.0.tar.gz
    # Actually use the docker approach for simplicity
    apt-get install -y -qq docker.io
    docker run -d --name vm -p 8428:8428 \
      -v /var/lib/victoria-metrics:/victoria-metrics-data \
      victoriametrics/victoria-metrics:v1.101.0 \
      -retentionPeriod=7d -httpListenAddr=:8428
  EOF

  labels = {
    role = "tsdb"
  }
}

# ─── Outputs ─────────────────────────────────────────────────

output "quorum_ips" {
  description = "External IPs of quorum nodes"
  value       = google_compute_instance.quorum[*].network_interface[0].access_config[0].nat_ip
}

output "quorum_internal_ips" {
  description = "Internal IPs of quorum nodes"
  value       = google_compute_instance.quorum[*].network_interface[0].network_ip
}

output "compute_ips" {
  description = "External IPs of compute nodes"
  value       = google_compute_instance.compute[*].network_interface[0].access_config[0].nat_ip
}

output "registry_ip" {
  description = "External IP of the registry"
  value       = google_compute_instance.registry.network_interface[0].access_config[0].nat_ip
}

output "registry_internal_ip" {
  description = "Internal IP of the registry"
  value       = google_compute_instance.registry.network_interface[0].network_ip
}

output "tsdb_ip" {
  description = "External IP of VictoriaMetrics"
  value       = google_compute_instance.victoriametrics.network_interface[0].access_config[0].nat_ip
}

output "ssh_command" {
  description = "SSH into quorum-1"
  value       = "ssh lattice@${google_compute_instance.quorum[0].network_interface[0].access_config[0].nat_ip}"
}
