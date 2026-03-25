#!/usr/bin/env bash
# setup.sh — Bootstrap the ssh-testlab environment.
#
# Generates a test SSH keypair (if not already present),
# builds the container image, and starts the lab.
#
# Usage:
#   ./setup.sh           # full setup + start
#   ./setup.sh --rebuild # force image rebuild
#   ./setup.sh --down    # tear everything down
#   ./setup.sh --reset   # tear down, remove keys, rebuild

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KEYS_DIR="${SCRIPT_DIR}/keys"
KEY_FILE="${KEYS_DIR}/id_ed25519"

usage() {
    echo "Usage: $0 [--rebuild|--down|--reset]"
    echo ""
    echo "  (no flag)   Generate keys if needed, build, and start."
    echo "  --rebuild   Force a fresh image build and restart."
    echo "  --down      Stop and remove containers."
    echo "  --reset     Full teardown: remove keys, rebuild from scratch."
}

generate_keys() {
    if [ -f "${KEY_FILE}" ]; then
        echo "[*] SSH keypair already exists at ${KEYS_DIR}/"
        return 0
    fi

    echo "[+] Generating ed25519 test keypair …"
    mkdir -p "${KEYS_DIR}"
    ssh-keygen -t ed25519 -f "${KEY_FILE}" -N "" -C "ssh-testlab"
    chmod 600 "${KEY_FILE}"
    chmod 644 "${KEY_FILE}.pub"
    echo "[+] Keypair created:"
    echo "    Private : ${KEY_FILE}"
    echo "    Public  : ${KEY_FILE}.pub"
}

lab_up() {
    cd "${SCRIPT_DIR}"
    echo "[+] Building and starting containers …"
    docker-compose up -d "$@"
    echo ""
    echo "============================================"
    echo "  ssh-testlab is running"
    echo "============================================"
    echo ""
    echo "  Password authentication (password: testpass):"
    echo "    ssh -p 2222 testuser@127.0.0.1"
    echo ""
    echo "  Key authentication:"
    echo "    ssh -p 2222 -i ${KEY_FILE} testuser@127.0.0.1"
    echo ""
    echo "  ProxyJump to target (password):"
    echo "    ssh -o ProxyJump=testuser@127.0.0.1:2222 testuser@target"
    echo ""
    echo "  ProxyJump to target (key):"
    echo "    ssh -i ${KEY_FILE} -o ProxyJump=testuser@127.0.0.1:2222 testuser@target"
    echo ""
    echo "  Telnet (login: testuser / testpass):"
    echo "    telnet 127.0.0.1 2323"
    echo ""
    echo "  Or add the snippet from ssh_config.example to ~/.ssh/config"
    echo "============================================"
}

lab_down() {
    cd "${SCRIPT_DIR}"
    echo "[-] Stopping and removing containers …"
    docker-compose down
    echo "[*] Done."
}

# ── Main ─────────────────────────────────────────────────────
case "${1:-}" in
    --rebuild)
        generate_keys
        lab_up "--build"
        ;;
    --down)
        lab_down
        ;;
    --reset)
        lab_down
        echo "[-] Removing keypair …"
        rm -rf "${KEYS_DIR}"
        generate_keys
        lab_up "--build" "--force-recreate"
        ;;
    --help|-h)
        usage
        ;;
    "")
        generate_keys
        lab_up "--build"
        ;;
    *)
        echo "Unknown option: $1" >&2
        usage >&2
        exit 1
        ;;
esac
