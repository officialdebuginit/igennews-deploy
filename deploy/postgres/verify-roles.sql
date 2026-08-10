\set ON_ERROR_STOP on

DO $verification$
DECLARE
  role_count integer;
  unsafe_count integer;
BEGIN
  SELECT count(*) INTO role_count
  FROM pg_roles
  WHERE rolname IN (
    'meridian_owner',
    'meridian_migrator',
    'meridian_app',
    'meridian_worker',
    'meridian_readonly'
  );
  IF role_count <> 5 THEN
    RAISE EXCEPTION 'one or more Meridian roles are missing';
  END IF;

  SELECT count(*) INTO unsafe_count
  FROM pg_roles
  WHERE rolname IN (
    'meridian_owner',
    'meridian_migrator',
    'meridian_app',
    'meridian_worker',
    'meridian_readonly'
  ) AND (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls);
  IF unsafe_count <> 0 THEN
    RAISE EXCEPTION 'a Meridian role has an unsafe cluster privilege';
  END IF;

  IF has_schema_privilege('meridian_app', 'meridian', 'CREATE')
    OR has_schema_privilege('meridian_worker', 'meridian', 'CREATE')
    OR has_schema_privilege('meridian_readonly', 'meridian', 'CREATE') THEN
    RAISE EXCEPTION 'a runtime role can create schema objects';
  END IF;
  IF NOT has_schema_privilege('meridian_migrator', 'meridian', 'CREATE') THEN
    RAISE EXCEPTION 'the migrator cannot create schema objects';
  END IF;
END
$verification$;

SELECT 'role contract verified' AS result;
