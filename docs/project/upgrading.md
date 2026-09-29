# Upgrading

## From 0.6 to 0.7: a project becomes a site

0.7 introduces **sites**: one server hosting many projects behind one list of
accounts, where before it served one project with accounts of its own. See
{doc}`../deploy/sites` for what a site is.

There is no automatic migration from a single project with accounts, on
purpose. `ridal server start` refuses a bare project and says so, rather than
reading it as a site and finding nothing. Moving one is a few commands:

1. Create the site next to the project:

   ```console
   $ ridal site init /srv/ridal --name "Glaciology 2026"
   ```

2. Make the first server administrator and send them the invite link:

   ```console
   $ ridal site account add jane --server-admin --path /srv/ridal
   ```

3. Move the project into the site, under a key:

   ```console
   $ mv /path/to/survey /srv/ridal/projects/survey
   ```

4. Recreate the other people as site accounts and add them as members of the
   project, with the role and download scope they had. Their **passwords do
   not carry over** — each account starts with a fresh invite link — but
   their interpretations do, since those are stored under the account name,
   not the account.

A project that still has the old per-project accounts is refused when a site
loads it, naming the file: 0.7 does not read a `users.json` that holds
passwords as a membership list. A project inside a site keeps its
`ridal_data/users.json`, but it now holds only members and the access policy.

The `ridal project user` commands are retired; they print an error pointing
at `ridal site account`. `ridal gui <project>` is unchanged, and still the
right way to work on one project locally.

## From a 0.6 project before the `ridal_data/` move

Projects made with an early 0.6 kept their state directly beside `ridal.toml`.
Ridal refuses to open such a project rather than showing it as empty;
`ridal project migrate` moves it into `ridal_data/`. This is unrelated to
sites and applies whether the project stays on its own or moves into one.

## From 0.6.0 to 0.6.1

A patch that only adds radargram and group identity keyword arguments to
`ridal.process` and `ridal.batch_process` (#218). Nothing else changes.
