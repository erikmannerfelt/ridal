# Changelog

## 0.7.0

**Sites.** One `ridal server start` can now host many projects from one
directory, behind one list of accounts.

- A **site** is a directory with a `ridal-site.toml`, a site-wide
  `accounts.json`, a signing key and a `projects/` directory of ordinary
  project directories. `ridal site init`, `ridal site account` and
  `ridal site project` manage it, and `ridal server start` takes a site
  rather than a project.
- Accounts are **site-wide**; a project holds **memberships** — a role and a
  download scope — and no passwords, so its directory stays portable. Account
  creation, password resets and server-administrator rights live at the site
  level; a project administrator manages only their own project's members,
  including creating an account for it and bulk invitations.
- Each project gets a card on the landing page with its member and radargram
  counts and a menu to rename, archive or delete it. Site settings hold the
  per-account site-wide theme, the accounts, a read-only memberships
  overview, and a **history** of account, membership and project changes.
- `ridal gui` still serves a single project with no accounts, exactly as
  before. The two modes are deliberately separate.

Project settings are split into **My site settings** (the theme) and
**My project settings**, and the old per-project account pages are replaced
by the site and member pages.

## 0.6.1

A patch cut from the 0.6.0 tag: `ridal.process` and `ridal.batch_process`
accept radargram and group identity keyword arguments (#218).

## 0.6.0

The first release with the browser GUI: the radargram catalog and viewer,
picking and interpretations, project accounts and roles, and `ridal render`.
