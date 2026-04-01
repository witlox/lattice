#!/bin/bash
# Setup a local OCI + ORAS registry for image testing.
set -euo pipefail

echo "=== Setting up OCI registry ==="

apt-get update -qq
apt-get install -y -qq docker.io

# Run registry:2 (OCI distribution spec, works for both Docker/Podman and ORAS)
docker run -d \
  --name registry \
  --restart always \
  -p 5000:5000 \
  -v /var/lib/registry:/var/lib/registry \
  registry:2

echo "=== Registry running on port 5000 ==="
echo "Push images: podman push --tls-verify=false localhost:5000/myimage:tag"
echo "Pull images: podman pull --tls-verify=false <this-ip>:5000/myimage:tag"
