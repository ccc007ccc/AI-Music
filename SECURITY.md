# Security Policy

## Supported versions

AI Music is currently pre-1.0. Security fixes are applied to the latest commit
on the default branch.

## Reporting a vulnerability

Please do not open a public issue for a vulnerability that could expose local
files, credentials, model-provider configuration, licensed assets, or allow a
model to bypass edit authorization.

Use GitHub's private vulnerability reporting for this repository. Include the
affected revision, reproduction steps, expected impact, and any proposed
mitigation. If private reporting is temporarily unavailable, open a minimal
issue asking the maintainer to enable a private contact channel without
publishing exploit details.

## Trust model

The built-in Codex adapter launches isolated, read-only, no-tool structured
model calls. This is not a security boundary for arbitrary replacement adapters
that already have unrestricted process or filesystem access. Hosts are
responsible for constraining the actual authority granted to external models.
