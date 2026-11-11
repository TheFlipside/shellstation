# ShellStation — PostgreSQL Administration Guide

This guide is intended for system administrators deploying ShellStation with a shared PostgreSQL backend. For architectural context on why RLS is used and how the permission model works, see [DESIGN.md §4.5](DESIGN.md#45-postgresql-multi-user-setup-row-level-security).

---

## Table of Contents

- [ShellStation — PostgreSQL Administration Guide](#shellstation--postgresql-administration-guide)
  - [Table of Contents](#table-of-contents)
  - [1. Initial Database Setup](#1-initial-database-setup)
  - [2. Creating Regular Users](#2-creating-regular-users)
    - [2.1 Group Role (Recommended)](#21-group-role-recommended)
    - [2.2 Per-User Grants (Alternative)](#22-per-user-grants-alternative)
  - [3. Resetting a User Password](#3-resetting-a-user-password)
  - [4. Removing a User](#4-removing-a-user)
  - [5. Applying Application Updates](#5-applying-application-updates)
  - [6. Backup and Restore](#6-backup-and-restore)
  - [7. Locks Held by a User](#7-locks-held-by-a-user)

---

## 1. Initial Database Setup

Connect to PostgreSQL as a superuser (e.g., `postgres`) and create the database and admin role:

```sql
-- Create the database
CREATE DATABASE shellstation;

-- Create the admin role (owns the schema, runs migrations)
CREATE ROLE shellstation_admin WITH LOGIN PASSWORD 'strong-random-password';
GRANT ALL PRIVILEGES ON DATABASE shellstation TO shellstation_admin;

-- On PostgreSQL 15+, grant schema permissions explicitly
\c shellstation
GRANT ALL ON SCHEMA public TO shellstation_admin;
```

Configure one ShellStation instance with the `shellstation_admin` credentials and start it once. This runs all migrations and sets up RLS policies.

---

## 2. Creating Regular Users

After the admin has initialized the schema, create roles for each team member. There are two permission models to choose from (see [DESIGN.md §4.5.2](DESIGN.md#452-permission-model) for the trade-offs):

- **Option A — Separated roles (recommended for larger teams):** Regular users get DML only. The admin must connect first after each ShellStation upgrade that includes schema changes.
- **Option B — All users can migrate (simpler for small teams):** Every user gets schema modification rights. Any user can apply migrations automatically — no admin coordination needed. Their table privileges include `TRIGGER` and `TRUNCATE`, which reach past row-level security. Choose this only for a team whose members you trust with each other's items.

### 2.1 Group Role (Recommended)

Using a group role avoids repeating grants for every new user. Create the group once, then add users to it.

**Option A — DML-only group (separated roles):**

```sql
\c shellstation

-- One-time: create the group role
CREATE ROLE shellstation_users NOLOGIN;
GRANT CONNECT ON DATABASE shellstation TO shellstation_users;
GRANT USAGE ON SCHEMA public TO shellstation_users;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO shellstation_users;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO shellstation_users;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO shellstation_users;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT USAGE ON SEQUENCES TO shellstation_users;

-- Per user: just add to the group
CREATE ROLE jdoe WITH LOGIN PASSWORD 'user-password';
GRANT shellstation_users TO jdoe;
```

**Option B — Group with migration rights (self-service updates):**

```sql
\c shellstation

-- One-time: create the group role with schema modification access
CREATE ROLE shellstation_users NOLOGIN;
GRANT CONNECT ON DATABASE shellstation TO shellstation_users;
GRANT USAGE, CREATE ON SCHEMA public TO shellstation_users;
GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA public TO shellstation_users;
GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public TO shellstation_users;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT ALL PRIVILEGES ON TABLES TO shellstation_users;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT ALL PRIVILEGES ON SEQUENCES TO shellstation_users;

-- Per user: just add to the group
CREATE ROLE jdoe WITH LOGIN PASSWORD 'user-password';
GRANT shellstation_users TO jdoe;
```

The PostgreSQL role name becomes the `owner` value in RLS — each user sees their own personal items and all shared items.

Two things to avoid:

- **A role named `local`.** That is the placeholder owner of rows from before RLS was set up. A role with that name would own all such rows, personal ones included. Nobody else can claim one once row-level security is on. Per-user mappings from before they had an owner carry the same placeholder (section 5). There a role with that name gets no more than any other role, but it cannot save a mapping of its own.
- **Granting `shellstation_admin` to regular users.** Members of the role that owns the tables have its rights over them. They get past the triggers, so they can put their rows into anyone's folder, and they can change the row-level security itself.

### 2.2 Per-User Grants (Alternative)

If you prefer not to use a group role (e.g., you need different privilege levels per user), you can grant permissions directly to each role.

**Option A — DML-only users (separated roles):**

```sql
\c shellstation

-- Create a regular user
CREATE ROLE jdoe WITH LOGIN PASSWORD 'user-password';

-- Grant connection access
GRANT CONNECT ON DATABASE shellstation TO jdoe;
GRANT USAGE ON SCHEMA public TO jdoe;

-- Grant DML on all existing tables
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO jdoe;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO jdoe;

-- Ensure future tables (from migrations) are also accessible
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO jdoe;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT USAGE ON SEQUENCES TO jdoe;
```

**Option B — Users with migration rights (self-service updates):**

```sql
\c shellstation

-- Create a user that can also run migrations
CREATE ROLE jdoe WITH LOGIN PASSWORD 'user-password';

-- Grant connection and schema modification access
GRANT CONNECT ON DATABASE shellstation TO jdoe;
GRANT USAGE, CREATE ON SCHEMA public TO jdoe;

-- Grant DML + DDL on all existing tables
GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA public TO jdoe;
GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public TO jdoe;

-- Ensure future tables are also fully accessible
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT ALL PRIVILEGES ON TABLES TO jdoe;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT ALL PRIVILEGES ON SEQUENCES TO jdoe;
```

Repeat for each user.

---

## 3. Resetting a User Password

```sql
ALTER ROLE jdoe WITH PASSWORD 'new-password';
```

The user must also update their password in ShellStation's settings (stored in the OS keychain, not the database).

To force a password change on next login (PostgreSQL 17+):

```sql
ALTER ROLE jdoe WITH PASSWORD 'temporary-password' VALID UNTIL '2026-04-17';
```

For older PostgreSQL versions, set a short expiry and coordinate with the user to update promptly.

---

## 4. Removing a User

Run these as a superuser (e.g., `postgres`). Row-level security applies to `shellstation_admin` as well, so as the admin the `UPDATE`s would reach none of the user's items. Once the triggers from section 5 are installed, only a superuser or the role that owns the tables (or a member of it) can change `owner`, apart from a user claiming a placeholder `'local'` row. If you leave a user's items orphaned instead, never create a role with that name again, because it would own them. Reassign a user's folders as whole trees. A user whose folder ends up below a folder of someone else's keeps what is in it and can rename it and move it out, but can put nothing new into it until then.

```sql
\c shellstation
-- Reassign their personal items to the admin (optional — or let them be orphaned)
UPDATE folders SET owner = 'shellstation_admin' WHERE owner = 'jdoe';
UPDATE sessions SET owner = 'shellstation_admin' WHERE owner = 'jdoe';

-- Their mappings of sessions to their own credential profiles and login sequences.
-- Mappings they never claimed are placeholders; see the end of section 5.
DELETE FROM session_credentials WHERE owner = 'jdoe';
DELETE FROM session_login_sequences WHERE owner = 'jdoe';

-- If you reassigned them, check that nothing still names them (both return 0)
SELECT count(*) FROM folders WHERE owner = 'jdoe';
SELECT count(*) FROM sessions WHERE owner = 'jdoe';

-- Revoke and drop
REVOKE shellstation_users FROM jdoe;
DROP ROLE jdoe;
```

---

## 5. Applying Application Updates

When a new ShellStation version includes database schema changes:

**Option A — Separated roles (admin applies migrations):**

1. **Stop all regular user instances** (or accept that they will fail to start until the migration is applied). A migration gives up if it has to wait more than 5 seconds for a lock, such as for a transaction another user has left open, or if one of its statements takes more than 60 seconds; ShellStation then says at startup which of the two it was, and tries again at the next start. See section 7 for finding what holds a lock, and for giving the owning role more time.
2. **Start one instance using the admin credentials** (`shellstation_admin`). Migrations run automatically on startup.
3. **If new tables were created**, re-run the default privilege grants (the `ALTER DEFAULT PRIVILEGES` from section 2 only covers tables created after the grant was set up — new tables from migrations executed by the admin role are covered).
4. **Regular users can now connect** normally.

**Option B — Self-service updates (any user can migrate):**

1. Users update ShellStation independently. The **first user to launch the new version** automatically applies pending migrations.
2. Subsequent users start normally — the migration check detects that all migrations are already applied.
3. No admin coordination is required. In the unlikely event of two users starting simultaneously during a migration, the migration system uses a lock to serialize execution — only one instance runs the migration, and the other waits for it, for up to 5 seconds. One that waits longer says at startup that it could not get a lock, and starts normally the next time.

**Row-level security changes (both options):** The RLS setup (policies, and the triggers that keep shared items with their owner) runs on every startup. Only the role that owns the tables can apply it, though. That is normally `shellstation_admin`, which created the schema in section 1. For any other role the setup ends at its check that the role owns the tables, and that failure is logged and skipped.

So when a release's notes mention a change to the RLS setup, start one instance with the owning role's credentials once after updating. This applies under Option B too. Until you do, the change is not in effect for anyone. For the release that adds these triggers, that means any user can still take over, move or delete other users' shared items. For the release that leaves shared items to their owner (below), it means any user can still rewrite them.

Under Option B, too, the triggers are installed by the owning role, as under Option A. They do not stop a user who holds `TRIGGER` or `TRUNCATE` on the tables, though, and Option B grants both ([DESIGN.md §4.5.1](DESIGN.md#451-how-rls-works)).

To check that the triggers are installed and enabled, run the query below. It does not show whether the functions behind them are current; starting the owning role once after an update takes care of that.

```sql
-- All six should be listed, with tgenabled = 'O'
SELECT tgrelid::regclass, tgname, tgenabled FROM pg_trigger
WHERE tgname IN ('folders_keep_owner', 'sessions_keep_owner',
                 'folders_keep_place', 'sessions_keep_place',
                 'folders_own_folder', 'sessions_own_folder');
```

If any is missing, start an instance with the owning role's credentials.

**Shared items (the release that leaves them to their owner):** Until this release any user could change another user's shared folder or session with a statement of their own, such as the host a shared session connects to, and could put a session of their own into another user's folder. From this release on only its owner changes a shared item, and an item goes only into a folder of its owner's ([DESIGN.md §4.5.1](DESIGN.md#451-how-rls-works)). ShellStation itself never did either. What users can notice is at connecting: a credential profile or login sequence that a session's own row names (Bulk Edit writes a login sequence there) now counts for that session's owner only, and a jump host that is another user's session needs the user's own credential mapping. The start of the owning role applies the rest. To check:

```sql
-- No rows: the two policies that let anyone update a shared item are gone
SELECT tablename, policyname FROM pg_policies
WHERE policyname IN ('folders_shared_update', 'sessions_shared_update');
```

The triggers query above shows the two new triggers. What was changed or put somewhere before this release stays as it is. A change to a shared item cannot be told from its owner's own, so owners should look over the sessions they share, their hosts and jump hosts above all. Items in a folder of another user's can be listed, as a superuser:

```sql
SELECT s.id, s.name, s.owner, f.name AS folder, f.owner AS folder_owner
FROM sessions s JOIN folders f ON f.id = s.folder_id WHERE s.owner <> f.owner;
SELECT c.id, c.name, c.owner, p.name AS folder, p.owner AS folder_owner
FROM folders c JOIN folders p ON p.id = c.parent_id WHERE c.owner <> p.owner;
```

**Per-user mappings (the release that gives them an owner):** Each user's mappings of sessions to their own credential profiles and login sequences (`session_credentials`, `session_login_sequences`) are protected by row-level security from this release on ([DESIGN.md §4.5.1](DESIGN.md#451-how-rls-works)). The release has a migration, so the owning role starts first, as above, and that start also applies the protection. To check it:

```sql
-- Both policies should be listed
SELECT tablename, policyname FROM pg_policies
WHERE policyname IN ('session_credentials_owner_all',
                     'session_login_sequences_owner_all');

-- Both tables should be listed, with a primary key that starts (session_id, owner, …).
-- No rows: the owning role has not started this release yet, or its setup failed.
SELECT conrelid::regclass, pg_get_constraintdef(oid) FROM pg_constraint
WHERE conname IN ('session_credentials_owner_key',
                  'session_login_sequences_owner_key');
```

Every user needs this release afterwards. An older version still connects with the mappings it has and can clear one, but can no longer save one. It also still connects with a placeholder (see below) that has not been claimed or deleted.

Mappings saved before this release stay in the tables with the placeholder owner `'local'`. Each user's ShellStation claims the ones under its own user identity at its next start, so users keep their assignments. Before this release any user could read and rewrite any mapping, so a mapping for a shared session cannot be checked for who wrote it. Until its user has claimed it, any user who can see that session can also still read the placeholder, and claim or delete it first. If you would rather not carry those over, delete the placeholders as a superuser. Do so after the owning role's first start of this release, which adds the `owner` column, and before the users start it. The owning role's own start claims the placeholders under its own user identity before you get to this; those are then its rows, not placeholders. The users then assign their credential profiles and login sequences again, which **Apply Credential Profile** and **Apply Login Sequence** on a folder do for all its sessions.

```sql
-- Optional: drop the mappings from before this release
DELETE FROM session_credentials WHERE owner = 'local';
DELETE FROM session_login_sequences WHERE owner = 'local';
```

Placeholders that nobody claims, such as those of a user who has left, can be deleted the same way at any later time.

---

## 6. Backup and Restore

```bash
# Backup (as a superuser: row-level security applies to shellstation_admin
# too, and pg_dump stops with an error rather than leave rows out)
pg_dump -h localhost -U postgres -d shellstation -F c -f shellstation_backup.dump

# Restore to a new database
createdb -h localhost -U postgres shellstation_restored
pg_restore -h localhost -U postgres -d shellstation_restored shellstation_backup.dump
```

RLS policies, constraints, and the migration tracking table are included in the dump. After restoring, the admin should connect once to verify the schema is intact.

---

## 7. Locks Held by a User

Every user of the database can hold a lock that other users' changes need, on purpose or not. A user can lock a whole table with the same privileges ShellStation itself needs, so those cannot be taken away. Single items a user can lock only where they are the user's own, with one exception: a row of a user's own that refers to another user's session keeps that session from being deleted, though not from being changed, while the transaction that wrote it is open. Such a row is a session that names the other as its jump host, for which its ID is enough, shared or not, a mapping for it, or a row in the central `credentials` table, which the app does not use and which has no row-level security. A transaction left open in `psql` or another tool does the same without meaning to, and so does a client that has stopped responding in the middle of a change. See [DESIGN.md §4.5.3](DESIGN.md#453-locks) for what ShellStation does about it.

**What users see.** A change that needs a lock someone else holds fails after 5 seconds with `canceling statement due to lock timeout`, and can be tried again. If a whole table is locked, the session tree does not load, for anyone and whoever owns the items, and ShellStation shows an error instead of waiting. Before this release it waited without end in both cases.

**Finding the session that holds the lock.** Run these as a superuser or a member of `pg_monitor`, since others see only their own sessions' details:

```sql
-- Who is waiting, and for whom
SELECT blocked.pid AS waiting_pid, blocked.usename AS waiting_user,
       blocker.pid AS holding_pid, blocker.usename AS holding_user,
       blocker.state, now() - blocker.xact_start AS open_for,
       blocker.query AS last_statement
FROM pg_stat_activity blocked
JOIN LATERAL unnest(pg_blocking_pids(blocked.pid)) AS b(pid) ON true
JOIN pg_stat_activity blocker ON blocker.pid = b.pid;

-- ShellStation gives up after 5 seconds, so there is often nobody waiting to
-- be seen. Transactions that have been open for more than a minute:
SELECT pid, usename, state, now() - xact_start AS open_for,
       query AS last_statement
FROM pg_stat_activity
WHERE datname = 'shellstation'  -- your database's name
  AND xact_start < now() - interval '1 minute'
ORDER BY xact_start;
```

**Ending it.** This closes that session and rolls back what it had not committed. It takes a superuser or a member of `pg_signal_backend`; `pg_monitor` only sees:

```sql
SELECT pg_terminate_backend(12345);  -- the pid from above
```

**Limits set on the server.** ShellStation's own sessions end a transaction they have left open for 30 seconds. Sessions from other tools have no such limit unless the server sets one:

```sql
-- Applies to sessions that connect from now on
ALTER DATABASE shellstation SET idle_in_transaction_session_timeout = '30s';

-- More time for the owning role's migrations (a role's setting overrides the database's):
-- to wait for a lock, and for a statement that takes long
ALTER ROLE shellstation_admin SET lock_timeout = '30s';
ALTER ROLE shellstation_admin SET statement_timeout = '10min';
```

ShellStation keeps a value the server sets for `lock_timeout`, `statement_timeout` or `idle_in_transaction_session_timeout` instead of its own (5 s, 60 s and 30 s), so set none of these to more than you want ShellStation to wait. A value of 0, which switches a limit off, is the exception: ShellStation cannot tell it from no value and sets its own. A limit set here covers what is left open by accident. It does not bind a user who means to hold a lock: every session can change these settings for itself. For that there is only finding the session, ending it, and, if need be, removing the user's access afterwards (`ALTER ROLE jdoe NOLOGIN;`, which does not end a session that is open, and section 4).
