#!/bin/sh
# entrypoint.sh — Prepare SSH environment and start sshd.

set -e

TEST_USER="${TEST_USER:-testuser}"
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
fi

echo "--- sshd_config in use ---"
grep -vE '^\s*#|^\s*$' /etc/ssh/sshd_config
echo "--------------------------"

# Run sshd in the foreground so the container stays alive.
exec /usr/sbin/sshd -D -e
