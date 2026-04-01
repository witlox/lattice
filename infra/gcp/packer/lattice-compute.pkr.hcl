// Packer template for lattice compute node image.
//
// Pre-installs podman, squashfs-tools, nsenter, and all runtime deps.
// Binary installation happens at deploy time via the provision bundle.
//
// Usage:
//   packer init .
//   packer build -var project_id=YOUR_PROJECT lattice-compute.pkr.hcl

packer {
  required_plugins {
    googlecompute = {
      version = ">= 1.1.0"
      source  = "github.com/hashicorp/googlecompute"
    }
  }
}

variable "project_id" {
  type = string
}

variable "zone" {
  type    = string
  default = "europe-west1-b"
}

source "googlecompute" "lattice-compute" {
  project_id          = var.project_id
  source_image_family = "ubuntu-2404-lts-amd64"
  zone                = var.zone
  machine_type        = "e2-standard-2"
  disk_size           = 50
  image_name          = "lattice-compute-{{timestamp}}"
  image_family        = "lattice-compute"
  image_description   = "Lattice compute node with podman, squashfs-tools, nsenter"
  ssh_username        = "packer"
}

build {
  sources = ["source.googlecompute.lattice-compute"]

  provisioner "shell" {
    inline = [
      "sudo apt-get update -qq",
      "sudo apt-get install -y -qq podman squashfs-tools squashfuse fuse3 util-linux curl jq python3",
      "sudo mkdir -p /opt/lattice/bin /opt/lattice/systemd /var/lib/lattice /var/cache/lattice/uenv /etc/lattice /scratch",
      "podman --version",
      "nsenter --version",
      "echo 'Lattice compute image ready'",
    ]
  }
}
