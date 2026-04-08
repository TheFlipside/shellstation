#!/usr/bin/env python3
"""Generate a ShellStation import JSON file with random sessions and folders.

Usage:
    python tools/generate_test_data.py              # 1000 sessions (default)
    python tools/generate_test_data.py -n 500       # 500 sessions
    python tools/generate_test_data.py -n 2000 -o big.json
"""

import argparse
import json
import random
import uuid
from pathlib import Path
from typing import Any

# --- Vocabulary for realistic-looking random data ---

TOP_FOLDERS = [
    "Production",
    "Staging",
    "Development",
    "Lab",
    "DR Site",
    "Office",
    "Cloud",
    "Legacy",
    "Monitoring",
    "DMZ",
]

SUB_FOLDERS = [
    "Web Servers",
    "App Servers",
    "Database Servers",
    "Load Balancers",
    "Firewalls",
    "Switches",
    "Routers",
    "Storage",
    "Hypervisors",
    "Kubernetes",
    "Docker Hosts",
    "CI/CD",
    "Logging",
    "DNS",
    "Mail Servers",
    "Bastion Hosts",
    "VPN Gateways",
    "Printers",
    "IPMI/BMC",
    "IoT Devices",
]

REGIONS = ["US-East", "US-West", "EU-West", "EU-Central", "APAC", ""]

HOSTNAMES = [
    "web",
    "app",
    "db",
    "cache",
    "proxy",
    "lb",
    "fw",
    "sw",
    "rtr",
    "mon",
    "log",
    "dns",
    "mail",
    "vpn",
    "jump",
    "bastion",
    "stor",
    "hyper",
    "kube",
    "ci",
    "nfs",
    "backup",
    "pgsql",
    "mysql",
    "redis",
    "rabbit",
    "kafka",
    "elastic",
    "grafana",
    "prometheus",
    "vault",
]

DOMAINS = [
    "internal.lan",
    "corp.local",
    "dc1.example.com",
    "dc2.example.com",
    "lab.test",
    "cloud.infra",
    "prod.internal",
    "stage.internal",
]

USERNAMES = [
    "root",
    "admin",
    "ubuntu",
    "ec2-user",
    "centos",
    "deploy",
    "ansible",
    "svc-monitor",
    "operator",
    "devops",
]

ICONS = [
    "desktop",
    "linux",
    "windows",
    "apple",
    "server",
    "switch",
    "router",
    "firewall",
    "database",
    "web",
    "cloud",
    "container",
    "wifi",
    "printer",
    "lock",
]

TAGS_POOL = [
    "linux",
    "windows",
    "ubuntu",
    "centos",
    "rhel",
    "debian",
    "critical",
    "non-critical",
    "monitored",
    "unmonitored",
    "docker",
    "k8s",
    "vm",
    "bare-metal",
    "aws",
    "azure",
    "gcp",
    "on-prem",
    "remote",
    "managed",
    "unmanaged",
]


def generate_folders(session_count: int) -> list[dict[str, Any]]:
    """Build a realistic folder hierarchy scaled to session count.

    Returns a flat list of folder dicts with parent_id references.
    """
    folders: list[dict[str, Any]] = []

    # Scale: roughly 1 top-level folder per 50 sessions, capped at pool size
    top_count = min(max(session_count // 50, 3), len(TOP_FOLDERS))
    top_names = random.sample(TOP_FOLDERS, top_count)

    for i, name in enumerate(top_names):
        folders.append(
            {
                "id": str(uuid.uuid4()),
                "name": name,
                "parent_id": None,
                "sort_order": i,
            }
        )

    # Add sub-folders under each top folder
    sub_names = list(SUB_FOLDERS)
    random.shuffle(sub_names)
    idx = 0
    for top in list(folders):
        remaining = len(sub_names) - idx
        if remaining < 2:
            break
        count = random.randint(2, min(5, remaining))
        for j in range(count):
            sub: dict[str, Any] = {
                "id": str(uuid.uuid4()),
                "name": sub_names[idx],
                "parent_id": top["id"],
                "sort_order": j,
            }
            folders.append(sub)
            idx += 1

            # Occasionally add a region sub-sub-folder
            region = random.choice(REGIONS)
            if region and random.random() < 0.3:
                folders.append(
                    {
                        "id": str(uuid.uuid4()),
                        "name": region,
                        "parent_id": sub["id"],
                        "sort_order": 0,
                    }
                )

    return folders


def random_ip() -> str:
    """Generate a random RFC 5737 / RFC 1918 IP address."""
    block = random.choice(["10", "172", "192"])
    ri = random.randint
    if block == "10":
        return f"10.{ri(0, 255)}.{ri(0, 255)}.{ri(1, 254)}"
    if block == "172":
        return f"172.{ri(16, 31)}.{ri(0, 255)}.{ri(1, 254)}"
    return f"192.168.{ri(0, 255)}.{ri(1, 254)}"


def random_hostname() -> str:
    """Generate a hostname like 'web-03.dc1.example.com' or a bare IP."""
    if random.random() < 0.4:
        return random_ip()
    prefix = random.choice(HOSTNAMES)
    num = random.randint(1, 99)
    domain = random.choice(DOMAINS)
    return f"{prefix}-{num:02d}.{domain}"


def random_tags() -> str:
    """Pick 0-3 random tags, comma-separated."""
    count = random.choices([0, 1, 2, 3], weights=[20, 40, 30, 10])[0]
    if count == 0:
        return ""
    return ",".join(random.sample(TAGS_POOL, count))


def generate_sessions(
    count: int,
    folders: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Generate random sessions distributed across folders.

    Returns (sessions, credentials).
    """
    # Collect leaf-ish folders (prefer deeper ones, but allow any)
    folder_ids = [f["id"] for f in folders]

    sessions: list[dict[str, Any]] = []
    credentials: list[dict[str, Any]] = []

    # Track sort_order per folder
    folder_sort: dict[str, int] = {}

    for _ in range(count):
        sid = str(uuid.uuid4())
        folder_id = random.choice(folder_ids)

        order = folder_sort.get(folder_id, 0)
        folder_sort[folder_id] = order + 1

        protocol = random.choices(["ssh", "telnet"], weights=[90, 10])[0]
        port = 22 if protocol == "ssh" else 23
        # Occasionally use a non-standard port
        if random.random() < 0.15:
            port = random.choice([2222, 8022, 2200, 10022, 443])

        auth = (
            "none"
            if protocol == "telnet"
            else random.choices(
                ["password", "publickey"],
                weights=[60, 40],
            )[0]
        )

        hostname = random_hostname()
        # Derive a display name from the hostname
        name = hostname.split(".")[0].replace("-", " ").title()

        session: dict[str, Any] = {
            "id": sid,
            "folder_id": folder_id,
            "name": name,
            "hostname": hostname,
            "port": port,
            "protocol": protocol,
            "username": random.choice(USERNAMES),
            "auth_method": auth,
            "jump_host_id": None,
            "tags": random_tags(),
            "icon": random.choice(ICONS),
            "sort_order": order,
        }
        sessions.append(session)

        # Create a credential entry for password/publickey sessions
        if auth != "none":
            cred_id = str(uuid.uuid4())
            secret = "" if auth == "password" else "/home/user/.ssh/id_ed25519"
            credentials.append(
                {
                    "id": cred_id,
                    "session_id": sid,
                    "username": session["username"],
                    "auth_type": auth,
                    "keychain_ref": f"session-{sid}",
                    "secret": secret,
                }
            )

    # Wire up some jump hosts: ~10% of SSH sessions use another as bastion
    ssh_sessions = [s for s in sessions if s["protocol"] == "ssh"]
    bastion_candidates = [
        s
        for s in ssh_sessions
        if any(kw in s["hostname"] for kw in ("jump", "bastion", "vpn"))
    ]
    if not bastion_candidates and len(ssh_sessions) > 10:
        bastion_candidates = random.sample(
            ssh_sessions,
            min(5, len(ssh_sessions)),
        )

    if bastion_candidates:
        jump_count = max(1, len(ssh_sessions) // 10)
        cap = min(jump_count, len(ssh_sessions))
        targets = random.sample(ssh_sessions, cap)
        for session in targets:
            bastion = random.choice(bastion_candidates)
            if bastion["id"] != session["id"]:
                session["jump_host_id"] = bastion["id"]

    return sessions, credentials


def main() -> None:
    """Entry point."""
    parser = argparse.ArgumentParser(
        description="Generate a ShellStation test dataset (JSON import file).",
    )
    parser.add_argument(
        "-n",
        "--count",
        type=int,
        default=1000,
        help="Number of sessions to generate (default: 1000)",
    )
    parser.add_argument(
        "-o",
        "--output",
        type=str,
        default="test_data.json",
        help="Output file path (default: test_data.json)",
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=None,
        help="Random seed for reproducible output",
    )
    args = parser.parse_args()

    if args.seed is not None:
        random.seed(args.seed)

    folders = generate_folders(args.count)
    sessions, credentials = generate_sessions(args.count, folders)

    data = {
        "folders": folders,
        "sessions": sessions,
        "credentials": credentials,
    }

    output = Path(args.output)
    output.write_text(json.dumps(data, indent=2), encoding="utf-8")

    print(
        f"Generated {len(folders)} folders, {len(sessions)} sessions, "
        f"{len(credentials)} credentials"
    )
    print(f"Written to {output.resolve()}")


if __name__ == "__main__":
    main()
