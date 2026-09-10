# Deploy to an Azure virtual machine

These operator steps create one small Ubuntu 24.04 virtual machine on Azure,
install the verified release package with the documented systemd units, expose
it through Caddy on a public hostname, and copy verified backups off the
virtual machine. They add Azure-specific provisioning to
[Linux release installation](linux-installation.md); that chapter remains the
authority for package verification, unit installation, upgrades, and removal.

The steps use the Azure CLI (`az`) from a workstation. A Standard_B1s virtual
machine (1 vCPU, 1 GiB) is sufficient: the accepted package peaked at 84.6 MiB
resident memory under the [capacity exercise](linux-capacity-evidence.md), and
Caddy adds a few tens of MiB. Azure free accounts include 750 hours per month
of B1s for the first 12 months; afterwards it costs a few dollars per month.
Prices and free allowances change, so confirm them in the Azure pricing pages
before relying on them.

## 1. Build or download the package

Follow [Build the release package](build-release-package.md). Keep the archive,
its `.sha256`, and the release run identity together.

## 2. Create the resource group and virtual machine

Sign in and choose a region and names. The public IP is static so DNS can point
at it permanently.

```sh
az login
az group create --name agent-coordinator --location eastus
az network public-ip create --resource-group agent-coordinator \
  --name coordinator-ip --sku Standard --allocation-method Static
az vm create --resource-group agent-coordinator --name coordinator \
  --image Ubuntu2404 --size Standard_B1s \
  --admin-username operator --ssh-key-values ~/.ssh/id_ed25519.pub \
  --public-ip-address coordinator-ip --os-disk-size-gb 30
az vm open-port --resource-group agent-coordinator --name coordinator \
  --port 80,443 --priority 1010
az network public-ip show --resource-group agent-coordinator \
  --name coordinator-ip --query ipAddress --output tsv
```

The `vm create` command opens SSH on port 22 by default. Port 80 is needed only
for Caddy's certificate challenge and redirect; the service port 8080 stays
closed because the service listens on loopback. Restrict the SSH rule to your
workstation address once the installation is complete:

```sh
az network nsg rule update --resource-group agent-coordinator \
  --nsg-name coordinatorNSG --name default-allow-ssh \
  --source-address-prefixes YOUR_WORKSTATION_IP/32
```

## 3. Point DNS at the address

Create an `A` record for the chosen hostname, for example
`coordinator.example.com`, at the static address printed above. Wait until
`dig +short coordinator.example.com` returns it from your workstation. Caddy
cannot obtain a certificate until the name resolves publicly.

## 4. Copy the package and prepare the host

```sh
scp dist/agent-coordinator-0.1.0-linux-x86_64.tar.gz \
    dist/agent-coordinator-0.1.0-linux-x86_64.tar.gz.sha256 \
    operator@coordinator.example.com:
ssh operator@coordinator.example.com
sudo apt-get update && sudo apt-get upgrade -y
sudo timedatectl set-ntp true
timedatectl
```

Confirm `System clock synchronized: yes`. Lease timing depends on service
time, and a clock rollback pauses new authority; see
[clock safety](clock-safety-contract.md).

## 5. Install the service

On the virtual machine, verify and extract the archive, then follow
[Install the service](linux-installation.md#install-the-service) without
change. In `/etc/agent-coordinator/service.env` set
`COORDINATOR_PUBLIC_ORIGIN=https://coordinator.example.com` and leave
`COORDINATOR_LISTEN=127.0.0.1:8080`. Initialize the administrator with the
hidden prompt, install the units, start the service, and run the first backup
and maintenance oneshots before enabling the timers, exactly as documented.

## 6. Install Caddy and enable HTTPS

Install Caddy from its official apt repository as documented at
[caddyserver.com/docs/install](https://caddyserver.com/docs/install), then
apply the repository's example configuration:

```sh
sudo install -o root -g root -m 0644 deploy/Caddyfile.example /etc/caddy/Caddyfile
sudo sed -i 's/coordinator\.example\.com/coordinator.example.com/' /etc/caddy/Caddyfile
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl enable --now caddy
sudo systemctl reload caddy
curl --fail --show-error https://coordinator.example.com/healthz
```

Replace the second hostname in the `sed` command with your real hostname. Caddy
obtains a public certificate from Let's Encrypt automatically; watch
`journalctl -u caddy -f` on the first start if the health check fails. The
hostname must equal the configured public origin exactly.

## 7. Copy verified backups off the virtual machine

The hourly backup timer writes local snapshots only. Azure Files gives a mounted
SMB destination that the documented copy procedure can use unchanged:

```sh
az storage account create --resource-group agent-coordinator \
  --name coordinatorbackups --sku Standard_LRS --kind StorageV2 \
  --min-tls-version TLS1_2 --allow-blob-public-access false
az storage share-rm create --resource-group agent-coordinator \
  --storage-account coordinatorbackups --name snapshots --quota 50
```

Storage account names must be globally unique lowercase letters and digits;
adjust `coordinatorbackups`. On the virtual machine, install `cifs-utils`,
store the account key in a root-only credentials file, and mount the share at
`/mnt/coordinator-backups` through `/etc/fstab` with `uid` and `gid` of the
`agent-coordinator` account and `nofail`. Azure's portal shows the exact mount
command for the share under **Connect**.

Then run the copy procedure in
[Verify and copy a snapshot off-server](backup-restore-guide.md#verify-and-copy-a-snapshot-off-server)
with `destination_repository=/mnt/coordinator-backups/agent-coordinator`, and
schedule it at least hourly. The destination is protected only after
destination-side verification passes; record that time separately from the
local snapshot time.

## 8. Complete the first operator tasks

1. Sign in at the HTTPS origin, create the project, and set its canonical
   repository key and required-check roster.
2. Issue one agent credential per workstation. The token is shown once.
3. From a bound checkout, run `agent-coordinator connect` and confirm the
   orientation response.
4. Rehearse a restore into a fresh directory from an off-server copy, following
   [the backup and restore guide](backup-restore-guide.md), and record the
   measured time. No host-loss protection is claimed until this passes.

## Upgrades, cost control, and removal

Upgrade with the boundary described in
[Upgrade and rollback boundary](linux-installation.md#upgrade-and-rollback-boundary).
Stopping the virtual machine with `az vm deallocate` stops compute charges but
keeps the disk, static address, and storage account billing. Delete the resource
group only after the off-server snapshots have been verified elsewhere; removal
of the package does not authorize deleting data or backups.
