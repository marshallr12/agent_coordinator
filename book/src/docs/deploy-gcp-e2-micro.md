# Deploy to a Google Cloud e2-micro

These operator steps create one e2-micro virtual machine on Google Compute
Engine, install the verified release package with the documented systemd units,
expose it through Caddy on a public hostname, and copy verified backups to a
Cloud Storage bucket. They add Google Cloud provisioning to
[Linux release installation](linux-installation.md); that chapter remains the
authority for package verification, unit installation, upgrades, and removal.

The e2-micro (2 shared vCPUs, 1 GiB) is included in Google Cloud's Always Free
tier when it runs in `us-west1`, `us-central1`, or `us-east1` with a standard
persistent disk of at most 30 GB, and the tier also covers a small amount of
Cloud Storage. The accepted package peaked at 84.6 MiB resident memory under
the [capacity exercise](linux-capacity-evidence.md), which used a different
two-CPU/4-GiB host; that result does not establish e2-micro capacity.
Free-tier terms change, so confirm them in Google's pricing pages before
relying on them. The steps use the `gcloud` CLI from a workstation.

Public IPv4 is billed separately, including addresses on free-tier VMs.
Google currently charges $0.005/hour after one free hour per month. An external
IPv6 address has no address charge. A proxied Cloudflare AAAA record can expose
an IPv6 origin to both IPv4 and IPv6 clients. Billing must still be enabled,
and compute, disk, backup storage, operations, and egress must remain within
their account-wide free allowances. Check [Google's free-tier limits](https://docs.cloud.google.com/free/docs/free-cloud-features)
and [network pricing](https://cloud.google.com/vpc/network-pricing).

## Installation record — 2026-09-14

The selected host is `agent-coordinator` in project `sithbit-19b44`, zone
`us-east1-b` (South Carolina): Ubuntu 24.04 x86-64, e2-micro, one 30-GB
`pd-standard` boot disk, and 1 GiB swap. The disk is preserved on instance
deletion and instance deletion protection is enabled. The reserved external
IPv6 address is `2600:1900:4020:671::`; the final interface has no public IPv4.
An IPv4 address was attached briefly for initial package installation and then
removed. Ubuntu package sources now use mirrors with IPv6 support.

The origin is [agents.sithbit.com](https://agents.sithbit.com); public HTTPS,
administrator login, secure cookies, CSRF rejection, and logout were verified.
The record is `AAAA agents 2600:1900:4020:671::`, proxied, with automatic TTL.
A hostname-specific Cloudflare configuration rule applies Full (strict) TLS
and disables Browser Integrity Check for API clients after Python's default
client received Cloudflare error 1010. The origin has a Let's Encrypt
certificate. Managing these records requires a
token scoped to `sithbit.com` with Zone DNS Edit, Zone Read, and Config Rules Edit.
Keep it in the operator's protected local configuration, never in the repository.
SSH is restricted
to Google IAP's IPv4 forwarding range; ports 80 and 443 are exposed over IPv6,
and the application listens only on `127.0.0.1:8080`. The custom VPC and subnet
are `agent-coordinator` and `agent-coordinator-us-east1`.

The installed package is built from deployment commit `019ffb6`
by [release run 34891009689](https://github.com/marshallr12/agent_coordinator/actions/runs/34891009689).
Its Linux package/build/systemd/HTTPS and native Windows jobs passed. The
deployment updates rustls to 0.23.45 for RUSTSEC-2026-0285; the server executable
is unchanged by that client TLS patch. Full Linux/native Windows coordination
checks and dependency audit passed in
[run 34891009230](https://github.com/marshallr12/agent_coordinator/actions/runs/34891009230).
See the live handoff for exact binary identities and validation scope.

The service uses the standard hourly backup and daily maintenance units.
The live artifact quota is 256 MiB, with a 2-GiB disk reserve; history and
backup growth still require monitoring. Local retention keeps the represented
24 hourly and 30 daily snapshots described in the backup contract.

The private bucket `gs://sithbit-19b44-agent-coordinator-backups-east` is in
`us-east1`, with uniform access and public-access prevention. The VM identity
`agent-coordinator-vm@sithbit-19b44.iam.gserviceaccount.com` has object-user access
on this bucket, without a service-account key or project-wide storage grant.

The deployment-specific helper `deploy/gcp-backup-transfer.py` is installed as
`/usr/local/sbin/agent-coordinator-backup-transfer`. Install its adjacent
`agent-coordinator-backup-transfer.service` and `.timer` into
`/etc/systemd/system`. The helper names this installation's bucket and paths;
edit them before reusing it elsewhere. It holds the source repository lock,
verifies and archives a complete snapshot, uploads it, downloads it again,
compares its SHA-256, and verifies the downloaded snapshot before saving an
atomic receipt or pruning remote archives absent from local retention. Its
root-only receipt is `/var/lib/agent-coordinator-transfer/last-verified.json`.
The timer runs hourly at five minutes past the hour. Inspect failed units and
receipt age; an enabled timer alone is not evidence of a successful transfer.
The bucket also retains soft-deleted objects for Google's default seven days,
which adds storage beyond the live snapshot set and counts toward billing.

Keep the bootstrap administrator password outside source control. The
administrator is `admin`; change its initial password after first sign-in.
This deployment does not enroll agents or bind any repository automatically.

The general IPv4-based installation instructions below remain an alternative.

## 1. Build or download the package

Follow [Build the release package](build-release-package.md). Keep the archive,
its `.sha256`, and the release run identity together.

## 2. Create the project resources and the virtual machine

```sh
gcloud auth login
gcloud config set project YOUR_PROJECT_ID
gcloud services enable compute.googleapis.com
gcloud compute addresses create coordinator-ip --region us-central1
gcloud compute instances create coordinator \
  --zone us-central1-a --machine-type e2-micro \
  --image-family ubuntu-2404-lts-amd64 --image-project ubuntu-os-cloud \
  --boot-disk-size 30GB --boot-disk-type pd-standard \
  --address coordinator-ip --tags coordinator-https
gcloud compute firewall-rules create coordinator-allow-https \
  --target-tags coordinator-https --allow tcp:80,tcp:443 \
  --source-ranges 0.0.0.0/0
gcloud compute addresses describe coordinator-ip --region us-central1 \
  --format 'value(address)'
```

Port 80 is needed only for Caddy's certificate challenge and redirect; the
service port 8080 stays closed because the service listens on loopback. SSH
uses `gcloud compute ssh`, which manages keys through the default network's
SSH rule. Restrict that rule to your workstation address once installation is
complete:

```sh
gcloud compute firewall-rules update default-allow-ssh \
  --source-ranges YOUR_WORKSTATION_IP/32
```

## 3. Point DNS at the address

Create an `A` record for the chosen hostname, for example
`coordinator.example.com`, at the static address printed above. Wait until
`dig +short coordinator.example.com` returns it. Caddy cannot obtain a
certificate until the name resolves publicly.

## 4. Copy the package and prepare the host

```sh
gcloud compute scp --zone us-central1-a \
  dist/agent-coordinator-0.1.0-linux-x86_64.tar.gz \
  dist/agent-coordinator-0.1.0-linux-x86_64.tar.gz.sha256 coordinator:~
gcloud compute ssh coordinator --zone us-central1-a
sudo apt-get update && sudo apt-get upgrade -y
timedatectl
```

Google images synchronize time from the metadata server; confirm
`System clock synchronized: yes`. Lease timing depends on service time; see
[clock safety](clock-safety-contract.md).

With 1 GiB of memory and no swap by default, add a small swap file so a
transient spike in the package manager or Caddy cannot trigger the kernel's
out-of-memory killer against the service:

```sh
sudo fallocate -l 1G /swapfile && sudo chmod 600 /swapfile
sudo mkswap /swapfile && sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

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

## 7. Copy verified backups to Cloud Storage

The hourly backup timer writes local snapshots only. Create a private bucket
in the same region and grant the virtual machine's service account object
write access:

```sh
gcloud storage buckets create gs://YOUR_UNIQUE_BUCKET --location us-central1 \
  --uniform-bucket-level-access --public-access-prevention
gcloud storage buckets add-iam-policy-binding gs://YOUR_UNIQUE_BUCKET \
  --member serviceAccount:VM_SERVICE_ACCOUNT_EMAIL --role roles/storage.objectCreator
```

The documented copy procedure needs a mounted destination for its staged,
verified, atomically published copy. Mount the bucket with Cloud Storage FUSE
(`gcsfuse`) at `/mnt/coordinator-backups` for the `agent-coordinator` account,
then run
[Verify and copy a snapshot off-server](backup-restore-guide.md#verify-and-copy-a-snapshot-off-server)
with `destination_repository=/mnt/coordinator-backups/agent-coordinator` at
least hourly. If a FUSE mount is not acceptable, verify the local snapshot, copy
it with `gcloud storage rsync` to a per-snapshot prefix, and verify the
destination copy again after download on another machine. Either way the
destination is protected only after destination-side verification passes;
record that time separately from the local snapshot time.

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
Publish the sanitized report and supporting verification excerpts using
[deployment evidence publication](linux-installation.md#publish-deployment-evidence).
Local deployment directories and private Cloud Storage receipts alone are not
retrievable by agents on other workstations.
A stopped instance keeps its disk and static address; a reserved static address
that is not attached to a running instance is billed even under the free tier.
Delete the instance and bucket only after the off-server snapshots have been
verified elsewhere; removal of the package does not authorize deleting data or
backups.
