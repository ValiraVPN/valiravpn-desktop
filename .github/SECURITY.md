# Security Policy

## Reporting a security vulnerability to ValiraVPN

If you believe you have found a security vulnerability, **DO NOT CREATE AN ISSUE**.
Instead, please send an email to security@grasandco.com, or file a report directly on
GitHub with [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing/privately-reporting-a-security-vulnerability).
Reports are treated with the highest priority and in confidence.

## Incident resolution process

After a report is submitted, the vulnerability is discussed privately, fixed, and then
publicly disclosed in a security advisory:

* Confirm the problem and determine the affected versions.
* Audit the code for similar problems elsewhere.
* Prepare fixes for every release still under maintenance, and ship them as fast as
  possible.
* Publicly disclose the problem in a security advisory.

## What this client protects

The WireGuard private key is generated on the machine and never leaves it. Only its
public half travels, when signing in creates the device. The session file holding the
account number, the token and that key is written readable only by the current user.

Bringing a tunnel up rewrites the routing table and requires administrator rights.
The relay is pinned to the physical gateway with a host route, so encrypted packets
cannot be routed back into the tunnel carrying them.
