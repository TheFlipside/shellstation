# ssh-testlab

Lightweight Docker-based SSH test environment with a jumphost and a target node.
Designed for developing and testing SSH terminal managers, ProxyJump workflows,
and authentication methods.

## Architecture

```text
┌─────────────┐       ┌─────────────────┐       ┌─────────────────┐
│  Host/       │ :2222 │  jumphost        │  :22  │  target          │
│  Workstation │──────►│  (TCP fwd: yes)  │──────►│  (TCP fwd: no)   │
│              │       │  Alpine + sshd   │       │  Alpine + sshd   │
└─────────────┘       └─────────────────┘       └─────────────────┘
                              │         sshlab bridge         │
                              └───────────────────────────────┘
```

- **jumphost** — exposed on `127.0.0.1:2222`, TCP forwarding enabled.
- **target** — no published ports, reachable only through the jumphost.

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
