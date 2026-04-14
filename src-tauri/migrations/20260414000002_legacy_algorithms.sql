-- Per-session opt-in for legacy SSH algorithms (weak kex / ciphers / MACs /
-- host key types). Off by default; enabling widens the preferred algorithm
-- list at connect time so russh can negotiate with old network gear that
-- only speaks diffie-hellman-group14-sha1, ssh-rsa, aes*-cbc, etc.
ALTER TABLE sessions
    ADD COLUMN legacy_algorithms INTEGER NOT NULL DEFAULT 0;
