# Security policy

## Supported versions

The most recent minor release gets security fixes. This project has no
long-term support branches.

| Version | Supported |
| ------- | --------- |
| 0.7.x   | Yes       |
| < 0.7   | No        |

## Report a vulnerability

Report vulnerabilities privately through
[GitHub Security Advisories](https://github.com/NavistAu/beachcomber/security/advisories/new).
Do not open a public issue for a vulnerability.

Include the version, your platform, a configuration that shows the problem, and
the effect you can demonstrate. Expect an acknowledgement within seven days.

## What is in scope

beachcomber runs as a daemon, listens on a Unix socket, and executes provider
commands on a timer. That shape defines the interesting attack surface:

- **Socket permissions.** The daemon's Unix socket should not be readable or
  writable by other users on a multi-user host. A permission or path weakness
  that lets another local user read cached state or inject responses is in
  scope.
- **Socket path handling.** Anything that lets an attacker pre-create,
  substitute, or symlink the socket path so a client connects to something else.
- **Cache poisoning.** A path by which an untrusted input causes the daemon to
  serve a value a consumer then treats as trusted. Consumers render cached
  values into shell prompts, so an injected value reaches a terminal.
- **Provider command execution.** Providers shell out — `git`, `mise`, `aws`,
  and the `script` provider run external commands. An input-dependent path that
  turns a cached value or configuration string into unintended command execution
  is in scope.
- **Terminal escape sequences.** A cached value containing control sequences
  that a prompt renders unescaped, since that can rewrite a user's terminal.

## What is out of scope

- **The `script` provider running what you configured it to run.** It executes
  commands by design. Configuring it to run something dangerous is not a
  vulnerability.
- **Untrusted repositories.** Reading state from a repository you do not trust
  means running `git` against it. That risk belongs to `git`, not to
  beachcomber.
- **Secrets appearing in cached values.** If a provider is configured to read
  something sensitive, it will be cached. Do not configure it to.
- **Vulnerabilities in the external commands providers invoke.** Report those
  upstream.
