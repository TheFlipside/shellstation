#!/bin/sh
# entrypoint-telnet.sh -- Start telnetd via inetutils-inetd.

set -eu

TEST_USER="${TEST_USER:-testuser}"

# Validate TEST_USER to prevent injection via environment variable.
case "${TEST_USER}" in
    *[!a-zA-Z0-9_-]*)
        echo "ERROR: Invalid TEST_USER '${TEST_USER}'" >&2
        exit 1
        ;;
esac

# Write inetd.conf for telnet service.
# Uses a shell wrapper because inetutils-inetd fails to exec telnetd directly.
echo "telnet stream tcp nowait root /usr/local/bin/telnetd-wrap telnetd-wrap" > /etc/inetd.conf

echo "Starting telnetd on port 23 via inetd (login as ${TEST_USER})"

# Run inetd in the foreground so the container stays alive.
exec inetutils-inetd -d
