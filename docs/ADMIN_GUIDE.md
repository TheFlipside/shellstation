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
    - [2.3 Databases Set Up with Option B](#23-databases-set-up-with-option-b)
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

-- Grant schema permissions explicitly (PostgreSQL 15+ gives none by default)
\c shellstation
GRANT ALL ON SCHEMA public TO shellstation_admin;

-- Before PostgreSQL 15 every role may create objects in the schema public,
-- and a database that was upgraded from such a version still lets them.
-- Take that away: an operator or function that a user puts there can be run
-- by other users' statements in place of the built-in one.
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
```

Configure one ShellStation instance with the `shellstation_admin` credentials and start it once. This runs all migrations and sets up RLS policies.

---

## 2. Creating Regular Users

After the admin has initialized the schema, create roles for each team member. Regular users get the privileges to read and write rows and no others: `SELECT`, `INSERT`, `UPDATE` and `DELETE`. Row-level security then keeps each user's personal items to that user ([DESIGN.md §4.5.2](DESIGN.md#452-permission-model)). Changes to the schema are the admin role's: after an upgrade of ShellStation that includes one, the admin role connects first (section 5).

Earlier versions of this guide offered a second permission model, Option B, under which every user was to apply migrations. It is withdrawn: PostgreSQL does not allow what it promised, and its grants reach past row-level security. If your database was set up that way, see section 2.3.

### 2.1 Group Role (Recommended)

Using a group role avoids repeating grants for every new user. Create the group once, then add users to it.

**DML-only group:**

```sql
\c shellstation

-- One-time: create the group role
CREATE ROLE shellstation_users NOLOGIN;
GRANT CONNECT ON DATABASE shellstation TO shellstation_users;
GRANT USAGE ON SCHEMA public TO shellstation_users;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO shellstation_users;
-- The table of applied migrations is the admin role's to write (section 5)
REVOKE INSERT, UPDATE, DELETE ON _sqlx_migrations FROM shellstation_users;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO shellstation_users;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO shellstation_users;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT USAGE ON SEQUENCES TO shellstation_users;

-- Per user: just add to the group
CREATE ROLE jdoe WITH LOGIN PASSWORD 'user-password';
GRANT shellstation_users TO jdoe;
```

The PostgreSQL role name becomes the `owner` value in RLS — each user sees their own personal items and all shared items.

Three things to avoid:

- **A role named `local`.** That is the placeholder owner of rows from before RLS was set up. A role with that name would own all such rows, personal ones included. Nobody else can claim one once row-level security is on. Per-user mappings from before they had an owner carry the same placeholder (section 5). There a role with that name gets no more than any other role, but it cannot save a mapping of its own.
- **Granting `shellstation_admin` to regular users.** Members of the role that owns the tables have its rights over them. They get past the triggers, so they can put their rows into anyone's folder, and they can change the row-level security itself.
- **More than the four privileges above.** `GRANT ALL PRIVILEGES` on the tables includes `TRIGGER` and `TRUNCATE`, which reach past row-level security: a trigger of a user's own runs when other users write, as those users, and `TRUNCATE` empties a table whatever its policies say. `CREATE` on the schema `public` lets a user put operators and functions there that other users' statements may run in place of the built-in ones. ShellStation needs none of these as a regular user.

### 2.2 Per-User Grants (Alternative)

If you prefer not to use a group role (e.g., you need different privilege levels per user), you can grant permissions directly to each role.

**DML-only users:**

```sql
\c shellstation

-- Create a regular user
CREATE ROLE jdoe WITH LOGIN PASSWORD 'user-password';

-- Grant connection access
GRANT CONNECT ON DATABASE shellstation TO jdoe;
GRANT USAGE ON SCHEMA public TO jdoe;

-- Grant DML on all existing tables
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO jdoe;
-- The table of applied migrations is the admin role's to write (section 5)
REVOKE INSERT, UPDATE, DELETE ON _sqlx_migrations FROM jdoe;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO jdoe;

-- Ensure future tables (from migrations) are also accessible
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO jdoe;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT USAGE ON SEQUENCES TO jdoe;
```

Repeat for each user.

### 2.3 Databases Set Up with Option B

Until this version the guide offered a second permission model, Option B, for teams that wanted every user to apply migrations without the admin. It gave each user, or the group, `CREATE` on the schema `public` and all privileges on the tables. It is withdrawn, for two reasons.

- **It did not do what it said.** PostgreSQL lets only a table's owner alter it, add an index to it or give it a policy, and no grant confers that. Every migration since the first changes an existing table, so an Option B user could apply none of them: that user's ShellStation started in offline mode until the admin role had started the new version, as a regular user's does.
- **Its grants reach past row-level security.** All privileges on a table include `TRIGGER` and `TRUNCATE` (see the things to avoid in section 2.1). A trigger of a user's own runs with the rights of whoever writes to the table, the admin role included. `CREATE` on the schema lets a user put operators and functions there that other users' statements may run. What [DESIGN.md §4.5.1](DESIGN.md#451-how-rls-works) says one user cannot do to another's items does not hold for a user with these grants.

**Do the following before the admin role connects again, in this order, with the users kept out.** A trigger runs with the rights of whoever writes to its table. One that a user added to `folders` or `sessions` can therefore run as the admin role whenever that role's ShellStation starts, since its start sends an update to both tables, and it runs as every other user who saves something, and as you if you change a row there. So until step 3 is done, change no row in ShellStation's tables yourself, as any role: no `INSERT`, `UPDATE`, `DELETE` or `COPY`, and no restore. None of the statements below does.

All of it is run as a superuser. Keep the users out first, and pin the search path for your own session, as this is the kind of database where someone may have put an operator in your way:

```sql
\c shellstation
SET search_path = pg_catalog, pg_temp;

-- Nobody but a superuser gets a new connection, and the open ones end
ALTER DATABASE shellstation CONNECTION LIMIT 0;
SELECT pg_terminate_backend(pid) FROM pg_stat_activity
WHERE datname = 'shellstation' AND pid <> pg_backend_pid();
```

If you want a record of what was there, take it now, since step 3 removes it: `pg_dump --schema-only` of the database.

**1. Take the grants back.** Run this once for the group role, and once more for every user that was given Option B directly, as section 2.2 had it, each in place of `shellstation_users`. It leaves a role with what sections 2.1 and 2.2 grant:

```sql
-- Before PostgreSQL 15, and in a database upgraded from then, everyone may
-- create objects in public, whatever a single role is granted (section 5)
REVOKE CREATE ON SCHEMA public FROM PUBLIC;

REVOKE CREATE ON SCHEMA public FROM shellstation_users;
REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM shellstation_users;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO shellstation_users;
REVOKE INSERT, UPDATE, DELETE ON public._sqlx_migrations FROM shellstation_users;
REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public FROM shellstation_users;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO shellstation_users;

-- The same for tables and sequences that later migrations create
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    REVOKE ALL PRIVILEGES ON TABLES FROM shellstation_users;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO shellstation_users;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    REVOKE ALL PRIVILEGES ON SEQUENCES FROM shellstation_users;
ALTER DEFAULT PRIVILEGES FOR ROLE shellstation_admin IN SCHEMA public
    GRANT USAGE ON SEQUENCES TO shellstation_users;
```

A `REVOKE` that fails over dependent privileges means the role passed a privilege on to another; run it again with `CASCADE` at its end. Then check that nothing is left, for any role, whether it holds it itself, through a group or as everyone does:

```sql
-- No rows: roles besides the admin role that can still create objects in
-- public or in the database
SELECT r.rolname FROM pg_roles r
WHERE NOT r.rolsuper AND r.rolname !~ '^pg_' AND r.rolname <> 'shellstation_admin'
  AND (has_schema_privilege(r.oid, 'public', 'CREATE')
       OR has_database_privilege(r.oid, current_database(), 'CREATE'));

-- No rows: what anyone but its owner holds on a table, view or sequence
-- beyond reading and writing rows, and beyond using a sequence
SELECT c.oid::regclass AS relation,
       CASE WHEN a.grantee = 0::oid THEN 'PUBLIC' ELSE a.grantee::regrole::text END AS grantee,
       a.privilege_type
FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace,
     LATERAL aclexplode(c.relacl) a
WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'
  AND a.grantee <> c.relowner
  AND NOT (c.relkind = 'S' AND a.privilege_type = 'USAGE')
  AND NOT (c.relkind <> 'S' AND a.privilege_type IN ('SELECT', 'INSERT', 'UPDATE', 'DELETE'));

-- No rows: privileges on single columns
SELECT a.attrelid::regclass AS relation, a.attname, a.attacl
FROM pg_attribute a
JOIN pg_class c ON c.oid = a.attrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE a.attacl IS NOT NULL AND n.nspname !~ '^pg_' AND n.nspname <> 'information_schema';

-- What later tables and sequences get: one row for the admin role's tables
-- in public and one for its sequences, each with an entry for every role you
-- grant to, arwd for tables and U for sequences, and nothing else
SELECT d.defaclrole::regrole AS for_role, d.defaclnamespace::regnamespace AS in_schema,
       d.defaclobjtype AS kind, d.defaclacl AS grants
FROM pg_default_acl d;
```

A member of the admin role is listed by the first of these whatever was revoked; step 2 finds the members. So is a role that owns the database or the schema `public` without being a superuser. An owner can grant itself anything there, so no revoke takes that from it: give the database and the schema to a superuser (`ALTER DATABASE shellstation OWNER TO postgres`, `ALTER SCHEMA public OWNER TO postgres`). Any other role that the first query lists for the database alone loses that with `REVOKE CREATE ON DATABASE shellstation FROM name`. And where the last query shows an entry or a row beyond the ones described, take it back with the statement that made it, `ALTER DEFAULT PRIVILEGES … REVOKE ALL PRIVILEGES ON TABLES FROM name`, for the role and schema the row names.

**2. Look for what was made with them.** Taking the grants back removes nothing that was made: a trigger that a user added keeps firing. Each of these should return no rows:

```sql
-- Tables, views, sequences and indexes that are not the admin role's, as all
-- of ShellStation's are. One a user created is that user's to alter, also
-- where a later migration finds it under the name of a table it would create.
SELECT c.oid::regclass AS relation, c.relkind, c.relowner::regrole AS owner
FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'
  AND c.relowner <> 'shellstation_admin'::regrole::oid;

-- Schemas besides public and the one of the setup's trigger functions
SELECT n.nspname, n.nspowner::regrole AS owner FROM pg_namespace n
WHERE n.nspname !~ '^pg_' AND n.nspname NOT IN ('information_schema', 'public')
  AND NOT (n.nspname = 'shellstation_rls'
           AND n.nspowner = 'shellstation_admin'::regrole::oid);

-- Triggers that do not call a function of the setup's: one in
-- shellstation_rls, with the schema and the function the admin role's
SELECT t.tgrelid::regclass AS on_table, t.tgname,
       t.tgfoid::regprocedure AS calls, f.proowner::regrole AS function_owner
FROM pg_trigger t
JOIN pg_proc f ON f.oid = t.tgfoid
JOIN pg_namespace n ON n.oid = f.pronamespace
WHERE NOT t.tgisinternal
  AND NOT (n.nspname = 'shellstation_rls'
           AND n.nspowner = 'shellstation_admin'::regrole::oid
           AND f.proowner = 'shellstation_admin'::regrole::oid);

-- Functions, operators and types outside the schema of the setup.
-- ShellStation keeps no function and no operator there, and no type but
-- the ones that come with its tables, which are the admin role's.
SELECT p.oid::regprocedure AS function, p.proowner::regrole AS owner,
       p.prosecdef AS runs_as_owner
FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
WHERE n.nspname !~ '^pg_' AND n.nspname NOT IN ('information_schema', 'shellstation_rls');
SELECT o.oid::regoperator AS operator, o.oprowner::regrole AS owner
FROM pg_operator o JOIN pg_namespace n ON n.oid = o.oprnamespace
WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema';
SELECT t.oid::regtype AS type, t.typowner::regrole AS owner
FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'
  AND t.typowner <> 'shellstation_admin'::regrole::oid;

-- Members of the admin role, directly or through another role, who have its
-- rights over the tables (section 2.1)
WITH RECURSIVE has(member) AS (
    SELECT m.member FROM pg_auth_members m
    WHERE m.roleid = 'shellstation_admin'::regrole::oid
    UNION
    SELECT m.member FROM pg_auth_members m JOIN has ON m.roleid = has.member
)
SELECT member::regrole AS member FROM has;
```

And three that should list only what you set up yourself. A role that bypasses row-level security reads and writes every user's items. Before PostgreSQL 16, one that may create roles can make itself a member of the admin role and set its password. The predefined roles that read and write the server's files or run its programs reach past the database altogether. And a setting stored for a role, such as a search path for the admin role, applies at each of its connections:

```sql
-- Your superusers and no one else
SELECT r.rolname, r.rolsuper, r.rolcreaterole, r.rolbypassrls FROM pg_roles r
WHERE r.rolname !~ '^pg_' AND (r.rolsuper OR r.rolcreaterole OR r.rolbypassrls);

-- Members of PostgreSQL's predefined roles
SELECT m.roleid::regrole AS predefined_role, m.member::regrole AS member
FROM pg_auth_members m WHERE m.roleid::regrole::text ~ '^pg_';

-- Settings stored for a role or a database
SELECT CASE WHEN s.setrole = 0::oid THEN 'every role' ELSE s.setrole::regrole::text END AS for_role,
       COALESCE(d.datname::text, 'every database') AS in_database, s.setconfig AS settings
FROM pg_db_role_setting s LEFT JOIN pg_database d ON d.oid = s.setdatabase;
```

And the table that says which migrations are applied, which every user could write to under Option B, as regular users could until the release that leaves it to the admin role (section 5). It should have one row for each migration of the newest ShellStation version the admin role has started, each with `success` true, and no other. Start that version once against an empty database of its own if you need something to compare with:

```sql
SELECT version, description, success, encode(checksum, 'hex') AS checksum
FROM public._sqlx_migrations ORDER BY version;
```

A row for a migration that the version does not have yet would keep that migration from being applied when its release comes.

Some rows are no user's doing. What you or an extension put into the database as a superuser is listed as the superuser's. And on a database whose admin role has not yet started the release that moved the trigger functions into `shellstation_rls` (section 5), the setup's own seven triggers are listed, with functions named `shellstation_…` in `public`; a function of such a name can also be left over from then, which section 5 has a query for. An extension that a user installed looks like one you installed, so go by what you know to have installed.

**3. Remove it.** A table that a user made goes first (`DROP TABLE name`); the block below stops at a partitioned one. Then every trigger and every policy on the tables in `public`, the setup's own with them. The check at each start goes by their names and not by what a policy says (section 5), so nothing short of making them again shows that they are the setup's. With row-level security on and no policy, a table takes nothing from a regular user and shows nothing, and step 4 makes the setup's seven triggers and eight policies again. The block switches row-level security on before it drops them, whatever was done to the tables:

```sql
ALTER TABLE public.folders ENABLE ROW LEVEL SECURITY, FORCE ROW LEVEL SECURITY;
ALTER TABLE public.sessions ENABLE ROW LEVEL SECURITY, FORCE ROW LEVEL SECURITY;
ALTER TABLE public.session_credentials ENABLE ROW LEVEL SECURITY, FORCE ROW LEVEL SECURITY;
ALTER TABLE public.session_login_sequences ENABLE ROW LEVEL SECURITY, FORCE ROW LEVEL SECURITY;

DO $$ DECLARE found record; BEGIN
    FOR found IN
        SELECT p.polname AS name, p.polrelid::regclass AS relation
        FROM pg_policy p
        JOIN pg_class c ON c.oid = p.polrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public'
    LOOP
        EXECUTE format('DROP POLICY %I ON %s', found.name, found.relation);
    END LOOP;
    FOR found IN
        SELECT t.tgname AS name, t.tgrelid::regclass AS relation
        FROM pg_trigger t
        JOIN pg_class c ON c.oid = t.tgrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public' AND NOT t.tgisinternal
    LOOP
        EXECUTE format('DROP TRIGGER %I ON %s', found.name, found.relation);
    END LOOP;
END $$;
```

Then what the queries of step 2 listed:

- A function, operator, type or schema that a user made: `DROP FUNCTION` and the like, with the name and arguments as the query prints them. Do not add `CASCADE`; what still depends on it is worth a look first.
- A table or view that a user made: drop it. Do not take it over as it is. A view, a default, a rule or a check on it is that user's code, and it would run as whoever uses the table. If you need what a table holds, copy the rows out, and make the table yourself.
- A member of the admin role: `REVOKE shellstation_admin FROM name`.
- An attribute, a predefined role or a stored setting that you did not set: `ALTER ROLE name NOBYPASSRLS` (or `NOCREATEROLE`), `REVOKE` the predefined role, and `ALTER ROLE name RESET ALL` for a setting of one role, with `IN DATABASE shellstation` after the name where it is for the one database. A setting for every role goes with `ALTER ROLE ALL RESET ALL`, with or without the database, and one for the database with `ALTER DATABASE shellstation RESET ALL`.
- A row in `_sqlx_migrations` for a migration that was never applied: `DELETE FROM public._sqlx_migrations WHERE version = …`, now that no trigger is left on the table. If a row is missing or marked as failed, the admin role's start stops at it: put that table's rows back from a backup.

Whatever a role still owns in the database also keeps it from being dropped later (section 4). `DROP OWNED BY name` drops everything the role owns in this database at once, tables with their rows included. It also takes back everything the role was granted directly there and on the database itself, the right to connect included, so for a user who was set up as in section 2.2, run all of that section's statements again afterwards, and check that the user is still in the group (`\du name`).

Run the queries of steps 1 and 2 once more. They should now return nothing but what you know to be yours.

**4. Start one instance with the admin role's credentials,** after letting connections in again:

```sql
ALTER DATABASE shellstation CONNECTION LIMIT -1;
```

Its start makes the setup's triggers and policies again and replaces the trigger functions. Its window shows no notice about row-level security once that is done; if it does, see section 5. The users can start after it.

**What these steps cannot undo, or show.** A trigger ran with the rights of whoever wrote, so what it did was done as that user:

- **Every user's items.** A trigger can have read what the user who wrote could read, and kept it somewhere. It can have changed rows as their owners saved them, the owner, visibility, host and jump host included, and been dropped again. Nothing in the database shows either afterwards. A backup from before the role had these grants is the one thing to compare with. Without one, every user should look over their own sessions, personal ones as well as shared ones, and the hosts and jump hosts above all.
- **The admin role.** If the admin role started or was used while such a trigger was there, statements may have run with its rights. It owns the tables, so they can have changed the policies and triggers, which step 3 therefore makes again, and the tables themselves: compare the output of `pg_dump --schema-only` with that of a database set up afresh if you want to be sure of them. They can also have granted privileges, changed what later tables are granted, made functions that run with the admin role's rights, added members to it and stored settings for it; steps 1 and 2 show what of that is there. And they can have changed the admin role's password, so set a new one (section 3).
- **A superuser.** If a superuser changed a row in those tables while such a trigger was there, or if the admin role is one, which section 1 does not make it, everything on the server was within that user's reach. The same goes, before PostgreSQL 16, for an admin role that may create roles.
- **`TRUNCATE`** emptied a table for everyone. With `folders` or `sessions` its users will have noticed; with the per-user mappings or the highlight profiles they may not have. Compare the row counts with a backup.

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

Run these as a superuser (e.g., `postgres`). Row-level security applies to `shellstation_admin` as well, so as the admin the `UPDATE`s would reach none of the user's items. Once the triggers from section 5 are installed, only a superuser or the role that owns the tables (or a member of it) can change `owner`, apart from a user claiming a placeholder `'local'` row. If you leave a user's items orphaned instead, never create a role with that name again, because it would own them. Reassign a user's folders as whole trees. A user whose folder ends up below a folder of someone else's keeps what is in it and can rename it and move it out, but can put nothing new into it until then. Reassign a user's sessions together as well, as the statements below do: a session and its jump host have to have the same owner to connect (section 5). If you delete a user's sessions instead, look first for sessions of other users that still name one of them as their jump host, with the query at the end of the jump host part of section 5: such a session is left without a jump host and connects directly from then on.

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

When a new ShellStation version includes database schema changes, the admin role applies them:

1. **Stop all regular user instances** (or accept that they will start in offline mode, with a banner that says the database could not be used, until the migration is applied). A migration gives up if it has to wait more than 5 seconds for a lock, such as for a transaction another user has left open, or if one of its statements takes more than 60 seconds; ShellStation then says at startup which of the two it was, and tries again at the next start. See section 7 for finding what holds a lock, and for giving the owning role more time.
2. **Start one instance using the admin credentials** (`shellstation_admin`). Migrations run automatically on startup.
3. **New tables need no grants of their own.** The `ALTER DEFAULT PRIVILEGES` from section 2 covers every table the admin role creates after it was set up, which a migration's tables are.
4. **Regular users can now connect** normally.

No other role can apply a migration: PostgreSQL leaves altering a table to its owner (section 2.3). Two instances of the admin role that start at the same time take turns, since the migration system holds a lock while it runs: the second waits for the first, for up to 5 seconds, and one that waits longer says at startup that it could not get a lock, and starts normally the next time.

**Row-level security changes:** The RLS setup (policies, and the triggers that keep shared items with their owner) runs on every startup. Only the role that owns the tables can apply it, though. That is normally `shellstation_admin`, which created the schema in section 1. For any other role the setup ends at its check that the role owns the tables, and that failure is logged and skipped.

So when a release's notes mention a change to the RLS setup, start one instance with the owning role's credentials once after updating. Until you do, the change is not in effect for anyone. For the release that adds these triggers, that means any user can still take over, move or delete other users' shared items. For the release that leaves shared items to their owner (below), it means any user can still rewrite them. For the release that keeps a jump host with its session's owner (below), it means a statement of a user's own can still name another user's session as a jump host; ShellStation itself already refuses to set one, and to connect through one.

**The check at each start:** ShellStation does not take the setup for granted. At every start, whichever role it is, it looks in the catalog for what its version's setup applies: row-level security on the four tables, the policies, the triggers and the functions they call, the constraint on visibility, and the primary key of the two mapping tables ([DESIGN.md §4.5.2](DESIGN.md#452-permission-model)). It also looks whether anyone but the owning role has been granted something on the schema of the trigger functions, or more than reading on the table of applied migrations, both of which the owning role's start revokes. Where something is missing, or a trigger function is not the one that version creates, a notice in the window says so and names the tables concerned. It stays until the user dismisses it and is back at every start while it applies, and the application log of that start lists every part as an error.

Every user of the database sees that notice, so expect to hear of it in two cases. After a release that changes the setup, until the owning role has started it once: start one instance with the owning role's credentials, and the users are no longer told from their next start on. And after a start of the owning role whose setup did not finish, such as one that gave up on a lock (section 7): its own window shows the notice too, and its next start that gets through applies the rest. A user on an older version than the one the owning role last started is told as well, since the trigger functions are then a later version's; that user updates. If the check itself fails, the window says that it could not check, and the application log says why.

The check goes by names, as the setup does. It does not read what a policy says, and it looks for no policy or trigger that someone has added beside the setup's.

The triggers do not stop a user who holds `TRIGGER` or `TRUNCATE` on the tables, as a user does who was set up under the withdrawn Option B ([DESIGN.md §4.5.1](DESIGN.md#451-how-rls-works)). Section 2.3 has how to take those back.

To look for yourself whether the triggers are installed and enabled and call the functions of the setup, run the query below. It does not show whether the functions are current, which the check at each start does; starting the owning role once after an update takes care of that.

```sql
-- All seven should be listed, with tgenabled = 'O', a function in the schema
-- shellstation_rls, and the owning role as that function's owner
SELECT t.tgrelid::regclass, t.tgname, t.tgenabled,
       t.tgfoid::regprocedure AS function, f.proowner::regrole AS function_owner
FROM pg_trigger t JOIN pg_proc f ON f.oid = t.tgfoid
WHERE t.tgname IN ('folders_keep_owner', 'sessions_keep_owner',
                   'folders_keep_place', 'sessions_keep_place',
                   'folders_own_folder', 'sessions_own_folder',
                   'sessions_own_jump_host');
```

If any is missing or differs, start an instance with the owning role's credentials.

**The schema of the trigger functions (from the release that adds it):** The functions the triggers call are kept in a schema of their own, `shellstation_rls`, which the owning role's start creates and which belongs to that role ([DESIGN.md §4.5.1](DESIGN.md#451-how-rls-works)). Regular users need no privilege on it, and should be given none: what anyone but the owning role has been granted on it is revoked at each start of the owning role. Creating it takes `CREATE` on the database, which section 1 grants the admin role. If the admin role does not have it, the application log of its start says `permission denied for database`, and the triggers stay as they were; create the schema once as a superuser, and start the admin role again:

```sql
CREATE SCHEMA shellstation_rls AUTHORIZATION shellstation_admin;
```

Earlier releases kept the functions in `public`. The start that moves the triggers to the new functions drops an old one with the last trigger that called it, if it is the starting role's to drop. One can be left there: one that no trigger of the setup's called at that start, and one that belongs to another role, such as a member of the owning role that ran an earlier release, or someone who created it under the setup's name on purpose. Nothing of the setup's calls either any more. To list and remove what is left, as a superuser:

```sql
SELECT oid::regprocedure AS function, proowner::regrole AS owner FROM pg_proc
WHERE pronamespace = 'public'::regnamespace::oid AND proname LIKE 'shellstation\_%';
-- then, for each one listed:
-- DROP FUNCTION public.shellstation_own_folder();
```

**Objects in `public` (existing databases):** A database that was set up before PostgreSQL 15, or by an earlier version of section 1, still lets every role create objects in the schema `public`. An operator or function a user puts there can be run by other users' statements in place of the built-in one, as those users. Take that away once, as a superuser; ShellStation creates nothing there as a regular user:

```sql
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
```

In a query you run yourself as a superuser, compare an `oid` column with a name only after casting the name on to `oid`, as the queries in this guide do (`'public'::regnamespace::oid`).

**Shared items (the release that leaves them to their owner):** Until this release any user could change another user's shared folder or session with a statement of their own, such as the host a shared session connects to, and could put a session of their own into another user's folder. From this release on only its owner changes a shared item, and an item goes only into a folder of its owner's ([DESIGN.md §4.5.1](DESIGN.md#451-how-rls-works)). ShellStation itself never did either. What users can notice is at connecting: a credential profile or login sequence that a session's own row names (Bulk Edit writes a login sequence there) now counts for that session's owner only, and a jump host that is another user's session needs the user's own credential mapping (see below for whose session a jump host can be). The start of the owning role applies the rest. To check:

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

**Jump hosts (the release that keeps them with their session's owner):** Until this release a session could name any session as its jump host, another user's included, and for a statement of a user's own the ID was enough, shared or not. The owner of a shared session thereby chose which of another user's sessions that user went through when opening it, and the owner of a jump host could take it from under another user's session by deleting it. From this release on a session's jump host is one of its owner's own sessions ([DESIGN.md §4.5.1](DESIGN.md#451-how-rls-works)). ShellStation offers no other for a new choice and accepts no other for a session that does not have it already, and it opens no connection through one, whatever the database holds. The start of the owning role adds the trigger that makes the database refuse it as well, `sessions_own_jump_host` in the triggers query above.

Sessions that name another user's session from before stay as they are. They no longer connect, and the message says which session names which. Their owner picks a jump host of their own instead; for a bastion that several users go through, each keeps a session of their own. Until then the jump host's owner can still delete it, and the session that named it then connects directly, to a host that was meant to be reached through a bastion, and nobody is told. Removing the jump host from these sessions for the users would do the same, so do not. What closes it is the session's owner picking a jump host of their own, so tell the owners the query lists, and ask the owners of the jump hosts to leave those in place until then. To list them, run this as a superuser: row-level security applies to `shellstation_admin` as well, which would see only its own and the shared sessions and get a list that is not complete, without an error. The query changes nothing.

```sql
SELECT s.id, s.name, s.owner, j.name AS jump_host, j.owner AS jump_host_owner
FROM sessions s JOIN sessions j ON j.id = s.jump_host_id WHERE s.owner <> j.owner;
```

A session whose owner is still the placeholder `'local'` is listed against a jump host that has an owner, and the other way round; ShellStation refuses those connections too.

**The table of applied migrations (the release that leaves it to the admin role):** ShellStation decides by the table `_sqlx_migrations` whether a start has migrations to apply. Until this release the grants of section 2 let every regular user write to it, as to any other table. A user who took a row away or marked one as failed sent every start into the migrations: regular users then started in offline mode, and the admin role's start failed, before it got to the row-level security setup, until someone put the table right by hand. A user who added a row for a migration of a later release had that migration taken for applied when the release came. From this release on the start of the owning role takes back whatever anyone but the table's owner holds beyond `SELECT`, on the table or on one of its columns, at every start, and the check at each start (above) names the table where someone still holds more, or where it is not the owning role's. Section 2 now has the statement for new roles; without it the notice shows for every user after you have granted to a new role, until the admin role has started again.

A user who can still write to the table can also lock all of it, and while it is locked the owning role's start does not get as far as taking the privileges back: it stops at reading the table and says at startup that it could not get a lock. End that user's session (section 7) and start again. Once the privileges are gone, a regular user can take no lock on the table that keeps anyone from reading it.

Update the users' ShellStation before, or together with, the admin role's start of this release. A version from before it writes to the table at its start where the checksums stored there have other line endings than its own build's, as a build for Windows has against a database that was set up from Linux or macOS, and the other way round. Refused that, it starts in offline mode. This release writes there only as the role that applies migrations.

To do it by hand, or to look, as a superuser:

```sql
-- For each role that was granted to; this takes what it holds on single
-- columns of the table as well
REVOKE ALL PRIVILEGES ON _sqlx_migrations FROM shellstation_users;
GRANT SELECT ON _sqlx_migrations TO shellstation_users;

-- No rows: what anyone but its owner holds on the table beyond reading it
SELECT CASE WHEN a.grantee = 0::oid THEN 'PUBLIC' ELSE a.grantee::regrole::text END AS grantee,
       a.privilege_type
FROM pg_class c, LATERAL aclexplode(c.relacl) a
WHERE c.oid = '_sqlx_migrations'::regclass::oid
  AND a.grantee <> c.relowner AND a.privilege_type <> 'SELECT';

-- No rows: privileges on single columns of it
SELECT t.attname, t.attacl FROM pg_attribute t
WHERE t.attrelid = '_sqlx_migrations'::regclass::oid AND t.attacl IS NOT NULL;
```

What was written to the table before is not undone, and cannot be told from it. Section 2.3 has a query for its rows and what they should be. A row for a migration that no version you have started carries is one to delete, as a superuser or the admin role. ShellStation writes a warning to the application log at each start while the table has a row for a migration that its own version does not carry. A version that is behind the one the admin role last started logs the same, so on a user's machine it means nothing by itself; in the log of the newest version's start it does.

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

Mappings saved before this release stay in the tables with the placeholder owner `'local'`. Each user's ShellStation claims the ones under its own user identity at its next start, so users keep their assignments. Before this release any user could read and rewrite any mapping, so a mapping for a shared session cannot be checked for who wrote it. Until its user has claimed it, any user who can see that session can also still read the placeholder, and claim or delete it first. If you would rather not carry those over, delete the placeholders as a superuser. Do so after the owning role's first start of this release, which adds the `owner` column, and before the users start it. The owning role's own start claims the placeholders under its own user identity before you get to this; those are then its rows, not placeholders. The users then assign their credential profiles and login sequences again, which **Apply Credential Profile** and **Apply Login Sequence** on a folder of their own do for all their own sessions in it and below it, a credential profile for those that are not Telnet; for another user's shared session it is **My Login**, one session at a time.

```sql
-- Optional: drop the mappings from before this release
DELETE FROM session_credentials WHERE owner = 'local';
DELETE FROM session_login_sequences WHERE owner = 'local';
```

Placeholders that nobody claims, such as those of a user who has left, can be deleted the same way at any later time.

**My Login (the release that adds it):** A user can assign a credential profile and a login sequence of their own to another user's shared session, in **My Login** in that session's context menu ([DESIGN.md §4.5.1](DESIGN.md#451-how-rls-works)). The release has no migration and changes nothing in the database, so no start of the owning role is needed for it. What a user assigns to a session that is not theirs is used only for what they confirmed the session names: protocol, host, port, user name, jump host and the switch for legacy algorithms. Of a jump host on the way it is the host, the port and the jump host it goes on to. If the owner of a shared session changes one of these, each other user who has assigned something to it, or to a jump host it goes through, is refused at the next connect, with a message that says what changed, and opens **My Login** once to look at it and save. Tell the users of a shared session before changing where it connects to. What a user confirmed is kept on their own machine, in `session-logins.json` in ShellStation's config directory, so a user is asked once more on a machine that does not have it. A user who assigned a profile to another user's session before this release is asked in the same way; the session dialog offered that until 0.11.3 kept it to the user's own sessions. A session that names what **My Login** cannot show for what it is cannot be confirmed, and the save says which session and why: for example a protocol other than SSH or Telnet or a port outside 1 to 65535, which only a statement of its owner's own can have written, or a host with a space or a backslash in it, or a user name with a space at its end, which its owner can set right in the session dialog.

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

Every user of the database can hold a lock that other users' changes need, on purpose or not. A user can lock a whole table with the same privileges ShellStation itself needs, so those cannot be taken away. Single items a user can lock only where they are the user's own, with one exception: a row of a user's own that refers to another user's session keeps that session from being deleted, though not from being changed, while the transaction that wrote it is open. Such a row is a mapping for it, or a row in the central `credentials` table, which the app does not use and which has no row-level security. Naming it as a jump host does this only for a session that named it before the database refused that (section 5), and where the trigger that refuses it is not installed yet or does not hold the user (one with the grants of Option B, section 2.3). A transaction left open in `psql` or another tool does the same without meaning to, and so does a client that has stopped responding in the middle of a change. See [DESIGN.md §4.5.3](DESIGN.md#453-locks) for what ShellStation does about it.

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
