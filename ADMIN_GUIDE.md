# ShellStation — PostgreSQL Administration Guide

This guide is intended for system administrators deploying ShellStation with a shared PostgreSQL backend. For architectural context on why RLS is used and how the permission model works, see [DESIGN.md §4.5](DESIGN.md#45-postgresql-multi-user-setup-row-level-security).

---

## Table of Contents

1. [Initial Database Setup](#1-initial-database-setup)
2. [Creating Regular Users](#2-creating-regular-users)
   - [2.1 Group Role (Recommended)](#21-group-role-recommended)
   - [2.2 Per-User Grants (Alternative)](#22-per-user-grants-alternative)
3. [Resetting a User Password](#3-resetting-a-user-password)
4. [Removing a User](#4-removing-a-user)
5. [Applying Application Updates](#5-applying-application-updates)
6. [Backup and Restore](#6-backup-and-restore)

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
- **Option B — All users can migrate (simpler for small teams):** Every user gets schema modification rights. Any user can apply migrations automatically — no admin coordination needed.

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

```sql
-- Reassign their personal items to the admin (optional — or let them be orphaned)
\c shellstation
UPDATE folders SET owner = 'shellstation_admin' WHERE owner = 'jdoe';
UPDATE sessions SET owner = 'shellstation_admin' WHERE owner = 'jdoe';
DELETE FROM session_credentials WHERE user_ident = 'jdoe';

-- Revoke and drop
REVOKE shellstation_users FROM jdoe;
DROP ROLE jdoe;
```

---

## 5. Applying Application Updates

When a new ShellStation version includes database schema changes:

**Option A — Separated roles (admin applies migrations):**

1. **Stop all regular user instances** (or accept that they will fail to start until the migration is applied).
2. **Start one instance using the admin credentials** (`shellstation_admin`). Migrations run automatically on startup.
3. **If new tables were created**, re-run the default privilege grants (the `ALTER DEFAULT PRIVILEGES` from section 2 only covers tables created after the grant was set up — new tables from migrations executed by the admin role are covered).
4. **Regular users can now connect** normally.

**Option B — Self-service updates (any user can migrate):**

1. Users update ShellStation independently. The **first user to launch the new version** automatically applies pending migrations.
2. Subsequent users start normally — the migration check detects that all migrations are already applied.
3. No admin coordination is required. In the unlikely event of two users starting simultaneously during a migration, the migration system uses a lock table to serialize execution — only one instance runs the migration, the other waits.

---

## 6. Backup and Restore

```bash
# Backup (as a user with read access, or the admin)
pg_dump -h localhost -U shellstation_admin -d shellstation -F c -f shellstation_backup.dump

# Restore to a new database
createdb -h localhost -U postgres shellstation_restored
pg_restore -h localhost -U postgres -d shellstation_restored shellstation_backup.dump
```

RLS policies, constraints, and the migration tracking table are included in the dump. After restoring, the admin should connect once to verify the schema is intact.
