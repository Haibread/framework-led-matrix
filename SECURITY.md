# Security policy

## Reporting a vulnerability

Report it privately through
[GitHub's private vulnerability reporting](https://github.com/Haibread/framework-led-matrix/security/advisories/new).
Only the maintainers and you see the report.

Do not open a public issue, pull request or discussion for a vulnerability: it
would be disclosed before it is fixed.

A useful report holds:

- the affected version or commit
- the steps to reproduce
- the impact as you see it

## Supported versions

The default branch is the only supported version.

## Scope

In scope:

- the `ledmat` binary, including the control socket it creates and the
  commands it accepts on it
- the state file it writes and reads back at startup
- the files in `packaging/`, which grant device access through a udev rule and
  run the daemon as a user service

Out of scope:

- a vulnerability in a dependency that is already public upstream: report it
  there, and Dependabot brings the fix in
- a finding that needs an already compromised host or account
- scanner output with no path to exploitation

## What happens next

1. The report is acknowledged and assessed.
2. A fix is prepared in the advisory's private fork.
3. The fix is released.
4. The advisory is published, with a CVE requested through GitHub when the
   impact warrants one, and you are credited unless you prefer otherwise.

Reports are handled on a best-effort basis.
