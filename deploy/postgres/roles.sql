\set ON_ERROR_STOP on

-- Required psql variables are intentionally explicit. Passwords must come from
-- a secret manager or an ephemeral operator environment, never this repository.
\if :{?owner_password}
\else
  \echo 'owner_password is required'
  \quit 1
\endif
\if :{?migrator_password}
\else
  \echo 'migrator_password is required'
  \quit 1
\endif
\if :{?app_password}
\else
  \echo 'app_password is required'
  \quit 1
\endif
\if :{?worker_password}
\else
  \echo 'worker_password is required'
  \quit 1
\endif
\if :{?readonly_password}
\else
  \echo 'readonly_password is required'
  \quit 1
\endif

DO $roles$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'meridian_owner') THEN
    CREATE ROLE meridian_owner;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'meridian_migrator') THEN
    CREATE ROLE meridian_migrator;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'meridian_app') THEN
    CREATE ROLE meridian_app;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'meridian_worker') THEN
    CREATE ROLE meridian_worker;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'meridian_readonly') THEN
    CREATE ROLE meridian_readonly;
  END IF;
END
$roles$;

ALTER ROLE meridian_owner LOGIN PASSWORD :'owner_password'
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE meridian_migrator LOGIN PASSWORD :'migrator_password'
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE meridian_app LOGIN PASSWORD :'app_password'
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE meridian_worker LOGIN PASSWORD :'worker_password'
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE meridian_readonly LOGIN PASSWORD :'readonly_password'
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;

CREATE SCHEMA IF NOT EXISTS meridian AUTHORIZATION meridian_owner;
ALTER SCHEMA meridian OWNER TO meridian_owner;
REVOKE ALL ON SCHEMA meridian FROM PUBLIC;

GRANT USAGE, CREATE ON SCHEMA meridian TO meridian_migrator;
GRANT USAGE ON SCHEMA meridian TO meridian_app, meridian_worker, meridian_readonly;

GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA meridian
  TO meridian_app, meridian_worker;
GRANT SELECT ON ALL TABLES IN SCHEMA meridian TO meridian_readonly;
GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA meridian
  TO meridian_app, meridian_worker;
GRANT SELECT ON ALL SEQUENCES IN SCHEMA meridian TO meridian_readonly;

ALTER DEFAULT PRIVILEGES FOR ROLE meridian_migrator IN SCHEMA meridian
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO meridian_app, meridian_worker;
ALTER DEFAULT PRIVILEGES FOR ROLE meridian_migrator IN SCHEMA meridian
  GRANT SELECT ON TABLES TO meridian_readonly;
ALTER DEFAULT PRIVILEGES FOR ROLE meridian_migrator IN SCHEMA meridian
  GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO meridian_app, meridian_worker;
ALTER DEFAULT PRIVILEGES FOR ROLE meridian_migrator IN SCHEMA meridian
  GRANT SELECT ON SEQUENCES TO meridian_readonly;

REVOKE CREATE ON SCHEMA public
  FROM meridian_migrator, meridian_app, meridian_worker, meridian_readonly;
