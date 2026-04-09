# ssh-testlab

Lightweight Docker-based SSH and Telnet test environment with a jumphost, an SSH
target, and a Telnet target. Designed for developing and testing terminal
managers, ProxyJump workflows, and authentication methods.

## Architecture

```text
┌─────────────┐       ┌─────────────────┐       ┌─────────────────┐
│  Host/       │ :2222 │  jumphost        │  :22  │  target          │
│  Workstation │──────►│  (TCP fwd: yes)  │──────►│  (TCP fwd: no)   │
│              │       │  Alpine + sshd   │       │  Alpine + sshd   │
│              │       └─────────────────┘       └─────────────────┘
│              │                │       sshlab bridge        │
│              │       ┌────────┴────────────────────────────┘
│              │ :2323 │  telnet-target    │
│              │──────►│  Alpine + telnetd │
└─────────────┘       └─────────────────┘
```

- **jumphost** — exposed on `127.0.0.1:2222`, TCP forwarding enabled.
- **target** — no published ports, reachable only through the jumphost.
- **cisco-sim** — exposed on `127.0.0.1:2223`, fake Cisco IOS CLI for testing keyword highlighting.
- **telnet-target** — exposed on `127.0.0.1:2323`, busybox telnetd with login.

## Quick Start

```bash
./setup.sh
```

This generates an ed25519 keypair under `keys/`, builds the image, and starts
both containers.

## Testing Authentication

### Password authentication

```bash
# Direct to jumphost
ssh -p 2222 testuser@127.0.0.1
# Password: testpass

# Through jumphost to target
ssh -o ProxyJump=testuser@127.0.0.1:2222 testuser@target
```

### Key authentication

```bash
# Direct to jumphost
ssh -p 2222 -i keys/id_ed25519 testuser@127.0.0.1

# Through jumphost to target
ssh -i keys/id_ed25519 -o ProxyJump=testuser@127.0.0.1:2222 testuser@target
```

### Force a specific auth method

```bash
# Force password only (ignore keys)
ssh -p 2222 -o PreferredAuthentications=password testuser@127.0.0.1

# Force key only (no password fallback)
ssh -p 2222 -o PreferredAuthentications=publickey -i keys/id_ed25519 testuser@127.0.0.1
```

## Testing Keyword Highlighting (Cisco Simulator)

```bash
# SSH into the fake Cisco IOS CLI
ssh -p 2223 testuser@127.0.0.1
# Password: testpass
```

The cisco-sim container drops you directly into a simulated Cisco IOS CLI.
Supported commands:

- `show version` / `sh ver` — IOS version, uptime
- `show interfaces` / `sh int` — Interface states (up/down/errors)
- `show ip interface brief` / `sh ip int br` — Interface summary table
- `show ip route` / `sh ip ro` — Routing table (C/S/O/B/D prefixes)
- `show ip bgp summary` / `sh ip bgp sum` — BGP neighbor states
- `show ip bgp` — BGP table entries
- `show ip ospf neighbor` / `sh ip ospf ne` — OSPF adjacency states
- `show access-lists` / `sh access` — ACLs with permit/deny
- `show logging` / `sh log` — Syslog with severity levels
- `show running-config` / `sh run` — Full running configuration
- `show environment` / `sh env` — Temperature/power/fan status
- `show cdp neighbors` / `sh cdp ne` — CDP neighbor table
- `show spanning-tree` / `sh span` — STP topology
- `ping <host>` — Simulated ICMP echo
- `traceroute <host>` — Simulated traceroute
- `enable` / `disable` — Toggle privileged mode prompt
- `?` or `help` — Command list

## Testing Telnet

```bash
# Direct to telnet-target (login prompt appears interactively)
telnet 127.0.0.1 2323
# Username: testuser
# Password: testpass
```

The Telnet container uses the same credentials as the SSH containers.

## SSH Config Snippet

Copy `ssh_config.example` into `~/.ssh/config` and adjust the `IdentityFile`
path. Then you can simply run `ssh lab-jumphost` or `ssh lab-target`.

## Management

```bash
./setup.sh              # start (build if needed)
./setup.sh --rebuild    # force image rebuild
./setup.sh --down       # stop and remove containers
./setup.sh --reset      # full teardown, regenerate keys, rebuild
```

## Customisation

### sshd configuration

Edit the files under `configs/` and restart:

```bash
docker compose restart
```

Key settings you might want to toggle:

| File                              | Setting                   | Default |
|-----------------------------------|---------------------------|---------|
| `configs/jumphost_sshd_config`    | `AllowTcpForwarding`      | `yes`   |
| `configs/jumphost_sshd_config`    | `PasswordAuthentication`  | `yes`   |
| `configs/target_sshd_config`      | `AllowTcpForwarding`      | `no`    |
| `configs/target_sshd_config`      | `PasswordAuthentication`  | `yes`   |

### Credentials

Change `TEST_USER` and `TEST_PASSWORD` in `docker-compose.yml` under the
`build.args` and `environment` sections, then rebuild:

```bash
./setup.sh --rebuild
```

## Resource Usage

Each container uses approximately 5–10 MB of RAM. The Alpine base image is
around 7 MB.
