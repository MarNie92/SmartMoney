# Deploying SmartMoney on an Oracle Cloud Always Free VM

Target: an Ampere A1 (aarch64) instance running Ubuntu, reachable only
over Tailscale. The daemon runs under systemd, restarts on failure and
survives reboots. SmartMoney stays read-only against Polymarket: it
places no real orders and holds no trading keys.

Everything below is free of charge within Oracle's Always Free tier.

## 1. Create the instance

1. Sign up at <https://www.oracle.com/cloud/free/>. A credit card is
   required for identity verification; Always Free resources are not
   billed. Pick a home region close to you (e.g. Frankfurt) — it cannot
   be changed later.
2. In the console: **Compute → Instances → Create instance**.
   - **Image**: Ubuntu 24.04 (Canonical Ubuntu).
   - **Shape**: *Ampere* → `VM.Standard.A1.Flex`. Always Free covers
     4 OCPUs and 24 GB RAM in total; using all four for this one
     instance is fine and makes the Rust build much faster.
   - **SSH keys**: upload the public key from step 2 below.
   - **Networking**: the default VCN is fine. Do **not** open any
     inbound ports beyond SSH (22); the dashboard is reached through
     Tailscale, not the public internet.
3. If you get *"Out of host capacity"*: that is Oracle being busy with
   free Ampere capacity, not an error on your side. Retry later, or try
   another availability domain in the same region. Trying a few times
   over a day usually works.

## 2. SSH key (on Windows)

In PowerShell:

```powershell
ssh-keygen -t ed25519 -C "smartmoney" -f $env:USERPROFILE\.ssh\oracle_smartmoney
type $env:USERPROFILE\.ssh\oracle_smartmoney.pub
```

Paste the printed public key into the instance's SSH key field. Then
connect (replace the IP with the instance's public address):

```powershell
ssh -i $env:USERPROFILE\.ssh\oracle_smartmoney ubuntu@<PUBLIC-IP>
```

## 3. Install

On the VM:

```bash
# Rust (needed to build; the toolchain stays in the ubuntu user's home)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

sudo apt-get update && sudo apt-get install -y git
git clone https://github.com/MarNie92/SmartMoney.git
cd SmartMoney
sudo ./deploy/install.sh
```

The first build takes roughly 10–20 minutes on four OCPUs. The script
creates a `smartmoney` system user, installs the binary to
`/opt/smartmoney/bin`, puts the database under `/var/lib/smartmoney`,
and registers the systemd service plus a nightly backup timer.

## 4. Secrets

```bash
sudo nano /etc/smartmoney/smartmoney.env
```

Fill in the same values you use locally:

| Variable | Value |
| --- | --- |
| `SMARTMONEY_TELEGRAM_BOT_TOKEN` | the BotFather token |
| `SMARTMONEY_TELEGRAM_ALLOWED_CHAT_IDS` | your chat id |
| `SMARTMONEY_ODDS_API_KEY` | the-odds-api.com key |
| `SMARTMONEY_DASHBOARD_TOKEN` | a long random string (see below) |
| `SMARTMONEY_DASHBOARD_BIND` | `100.x.y.z:8080` (the Tailscale address) |

Generate a dashboard token with `openssl rand -hex 24`.

Without a `config.toml` the daemon uses its built-in defaults, which
are what `config.example.toml` documents — that is the intended setup
here. To override them, copy a config to the VM and point
`SMARTMONEY_CONFIG` at it.

The file is mode 640, owned by `root:smartmoney`. Keep it out of git —
it is not part of the repository.

Then start the service:

```bash
sudo systemctl enable --now smartmoney
journalctl -u smartmoney -f
```

## 5. Tailscale (private access to the dashboard)

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up
```

Follow the printed URL to authenticate, then install Tailscale on your
phone and laptop and sign in with the same account. `tailscale ip -4`
prints the VM's `100.x.y.z` address — put it into
`SMARTMONEY_DASHBOARD_BIND` (with `:8080`) and restart the service.

The dashboard is then at `http://100.x.y.z:8080/?token=<your token>`
from any of your Tailscale devices, and unreachable from the public
internet. The daemon refuses to bind to a non-loopback address without
a token, so a misconfiguration fails loudly instead of exposing the
page.

## 6. Everyday commands

```bash
systemctl status smartmoney           # is it running?
journalctl -u smartmoney -f           # live log
journalctl -u smartmoney | grep PAPER # paper trades so far
sudo systemctl restart smartmoney     # after an env change

# Deploy a new version
cd ~/SmartMoney && git pull && sudo ./deploy/install.sh

# Backups (nightly at 03:30 UTC, seven kept)
systemctl list-timers smartmoney-backup
ls -lh /var/lib/smartmoney/backups
```

To copy a backup to your PC (run in PowerShell, not on the VM):

```powershell
scp -i $env:USERPROFILE\.ssh\oracle_smartmoney `
    ubuntu@<PUBLIC-IP>:/var/lib/smartmoney/backups/*.gz .
```

Reading the backup directory requires the `smartmoney` group; add
yourself once with `sudo usermod -aG smartmoney ubuntu` and reconnect.

## 7. Moving the existing database (optional)

The local database already holds collected trades and paper positions.
To continue with it instead of starting fresh, stop the local daemon,
then from Windows PowerShell:

```powershell
scp -i $env:USERPROFILE\.ssh\oracle_smartmoney `
    C:\Users\marce\Desktop\polymarket\smartmoney.db ubuntu@<PUBLIC-IP>:/tmp/
```

On the VM:

```bash
sudo systemctl stop smartmoney
sudo install -o smartmoney -g smartmoney -m 640 /tmp/smartmoney.db /var/lib/smartmoney/smartmoney.db
sudo rm /tmp/smartmoney.db
sudo systemctl start smartmoney
```

Run only one daemon at a time against the same strategy accounts —
two instances would both react to the same signals.
