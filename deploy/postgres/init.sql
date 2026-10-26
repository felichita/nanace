-- Runs once on a fresh volume as the bootstrap superuser.
-- The application connects as `nanace`: a non-superuser without BYPASSRLS,
-- so row-level security applies to it. Development credentials only.
CREATE ROLE nanace LOGIN PASSWORD 'nanace' NOSUPERUSER NOBYPASSRLS NOCREATEDB;
CREATE ROLE nanace_tenant NOLOGIN NOINHERIT;
GRANT nanace_tenant TO nanace;
CREATE DATABASE nanace OWNER nanace;
