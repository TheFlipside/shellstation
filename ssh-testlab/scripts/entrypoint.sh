#!/bin/sh
# entrypoint.sh — Prepare SSH environment and start sshd.

set -eu

TEST_USER="${TEST_USER:-testuser}"

# Validate TEST_USER to prevent injection via environment variable.
case "${TEST_USER}" in
    *[!a-zA-Z0-9_-]*)
        echo "ERROR: Invalid TEST_USER '${TEST_USER}' — only alphanumeric, dash, and underscore allowed" >&2
        exit 1
        ;;
esac

AUTH_KEYS="/home/${TEST_USER}/.ssh/authorized_keys"

# If a public key was mounted, install it for the test user.
if [ -f /tmp/ssh-pubkey/id_ed25519.pub ]; then
    cp /tmp/ssh-pubkey/id_ed25519.pub "${AUTH_KEYS}"
    chmod 600 "${AUTH_KEYS}"
    chown "${TEST_USER}:${TEST_USER}" "${AUTH_KEYS}"
fi

# Use custom sshd_config if mounted, otherwise keep the default.
if [ -f /tmp/sshd_config/sshd_config ]; then
    cp /tmp/sshd_config/sshd_config /etc/ssh/sshd_config
    # Validate the config before starting sshd.
    if ! sshd -t -f /etc/ssh/sshd_config; then
        echo "ERROR: Mounted sshd_config is invalid" >&2
        exit 1
    fi
fi

echo "--- sshd_config in use ---"
grep -vE '^\s*#|^\s*$' /etc/ssh/sshd_config
echo "--------------------------"

# Run sshd in the foreground so the container stays alive.
exec /usr/sbin/sshd -D -e
