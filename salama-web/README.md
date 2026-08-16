# Salama Web VPN (`salama-web`)

A self-hostable **WireGuard VPN server + admin web UI**, packaged to deploy on
**Coolify** as an isolated Docker Compose app. It gives you a real, working VPN
you connect to with any WireGuard client (macOS/iOS/Android/Windows/Linux) — the
exit server the Salama client will eventually dial.

It wraps [`wg-easy`](https://github.com/wg-easy/wg-easy) (a mature, widely-run
WireGuard image) instead of a hand-rolled server — the safe, boring choice for
something that terminates your traffic.

---

## Will it break my server / other deployments? No — by design

| Concern | Why it's safe here |
|---|---|
| Host networking | Uses **bridge** networking, never `network_mode: host`. Its own Compose network. |
| `ip_forward` / sysctls | Docker **namespaces** these to the container — the host's `/proc/sys` and other containers are untouched. |
| Firewall / NAT | wg-easy does NAT **inside its own network namespace**; your host `iptables` are not modified. |
| Disk | State is a **named volume** (`salama_wg`), not a bind-mount into system paths. |
| Ports | Publishes exactly **one UDP port** (configurable) + one proxied web route. Change `WG_PORT` if `51820` is taken. |
| Kernel | `SYS_MODULE` loads the `wireguard` module only if it isn't already present (benign; most VPS have it). Drop that cap if your host already has WireGuard and you want zero host interaction. |

Nothing else on the box is affected. Removing the Coolify app removes the
container + network; delete the `salama_wg` volume to wipe all VPN state.

---

## What you actually have to do (everything else is automated)

Exactly **four one-time manual steps** — nothing else. WireGuard key
generation, NAT, interface bring-up, per-device client configs, health,
restart, and (if you wire the secret) redeploy-on-push are all automatic.

1. **Create the Coolify resource** — New Resource → Docker Compose → point at
   this `salama-web/` folder. (One-time; Coolify then owns the lifecycle.)
2. **Set two env vars** in Coolify's UI:
   - `WG_HOST` — this server's **public IP or a domain** that resolves to it
     (clients dial this for the tunnel).
   - `PASSWORD_HASH` — the admin bcrypt hash. Run `./gen-password.sh` (prompts
     silently, no shell-history leak) and paste the printed `PASSWORD_HASH=`
     value. `WG_PORT` / `WG_DEFAULT_DNS` / `LANG` have working defaults.
3. **Give the web UI a domain** — assign the `salama-vpn` service a domain in
   Coolify pointed at container **port `51821`**. Coolify's proxy (Traefik)
   gives it TLS and keeps the admin panel off the raw internet (it's only
   `expose`d, never host-published). This is a DNS/domain step only you can do.
4. **Open the tunnel UDP port** — allow inbound **`${WG_PORT}/udp`** (default
   `51820/udp`) on the server's firewall **and** the cloud provider's security
   group. ⚠️ This is the #1 "connects to nothing, no error" cause: Coolify's
   proxy handles the HTTPS web route, but it does **not** proxy the UDP tunnel —
   the WireGuard port must be reachable directly. This depends on your host/
   provider, so it can't be scripted from here.

Then **Deploy**. Open the web UI, log in, click **New Client**, scan the QR /
import the `.conf` — done.

Steps 3 and 4 are the only irreducibly-manual parts: they depend on *your*
domain, DNS, and *your* provider's firewall, which live outside this repo.

### Set the admin password

```bash
./gen-password.sh            # prompts silently, prints PASSWORD_HASH=...
./gen-password.sh --env      # ...and writes it into ./.env
```

Or by hand:

```bash
docker run --rm ghcr.io/wg-easy/wg-easy:14 wgpw 'your-strong-password'
# prints: PASSWORD_HASH='$2a$12$....'
```

Paste the hash **exactly as printed — raw, single `$`** — into either the Coolify
env UI or a local `.env`. This compose reads it via `${PASSWORD_HASH}`, so **do
not double the `$`.** (The common "double every `$` to `$$`" advice applies only
when you write the hash *inline* in a compose file; we don't.)

---

## Connect

1. Install WireGuard (App Store / [wireguard.com/install](https://www.wireguard.com/install/)).
2. In the Salama web UI → **New Client** → scan the QR (mobile) or import the
   downloaded `.conf` (desktop).
3. Toggle the tunnel on — your traffic now exits via this server, and DNS goes to
   `WG_DEFAULT_DNS` (Quad9 by default).

## How this fits the Tabibu "Salama" client

Tabibu ships **Salama** (in-app encrypted DNS today; a full IP-hiding VPN
planned). `salama-web` is the **exit server** side of that: a place your traffic
can egress from under an IP that isn't your ISP's. For now, connect with a
standard WireGuard client; the native Salama VPN client will target servers like
this one once its data-plane (`tabibu-tunnel`) ships. See
`../tabibu-vpn-notes/vpn-server-runbook.md` for the full multi-PoP design.

## CI/CD

`.github/workflows/salama-web.yml` runs on any change under `salama-web/`:

- **`validate`** (unit-level, every push/PR): `docker compose config`, enforces a
  **UDP** tunnel port and a **pinned** image (no `:latest`/untagged), checks
  `.env.example` documents every variable, and that no real `.env` is committed.
- **`yamllint`**: lints both compose files.
- **`e2e`** (every push/PR): the real regression gate. Brings the container up,
  waits for healthy, asserts `wg0` is up on the tunnel port and the admin web UI
  returns `200`, then **creates a client through the admin API, spins up a
  separate WireGuard client container on the same network, and proves a real
  cryptographic handshake** — ICMP flows through the tunnel to the server and
  `wg` reports a completed handshake. Finally tears everything down (`down -v`,
  no leftovers). A broken image, compose, or tunnel fails here, not in prod.
- **`deploy`** (push to `main`): triggers a **Coolify deploy** — gated on all
  three checks above, and only if you've set the deploy secret; otherwise it
  skips cleanly (so PRs/forks never fail).

`docker-compose.ci.yml` is a test-only overlay that publishes the web port so
the integration job can curl it; it is never used in production.

To enable auto-deploy: in Coolify, copy the resource's **Deploy Webhook** URL,
then add it to the GitHub repo as the secret **`SALAMA_COOLIFY_WEBHOOK`** (and
**`SALAMA_COOLIFY_TOKEN`** if your Coolify webhook needs a bearer token). Every
push to `main` that touches `salama-web/` will then redeploy.

## Operate

- **Logs:** Coolify → the resource → Logs (or `docker logs salama-vpn`).
- **Health:** the container reports healthy once `wg` is up.
- **Update:** re-pull `ghcr.io/wg-easy/wg-easy:14` and redeploy (pinned tag — no surprise upgrades).
- **Reputation:** datacenter IPs often trip CAPTCHAs / streaming blocks — that's
  the IP range, not this config.
- **Legal:** running a VPN service can carry logging/registration duties in some
  jurisdictions. This is a tool for your own use; get local advice before
  offering it to others.
