# Security Policy

## Reporting a Vulnerability

Please do not file a public issue for a security problem. Report it privately instead.

On this repository's GitHub page, open the **Security** tab and choose **Report a vulnerability**. The
report stays visible only to the maintainers until a fix ships.

## Information to Provide

- What the vulnerability is
- How to reproduce it
- Which versions are affected
- What an attacker could achieve

## Scope

In scope:

- The `headroom` and `headroom-app` binaries and the GNOME Shell extension
- The built-in providers, including how they handle credentials and call APIs
- Credential storage in the Secret Service and the handling of hub management keys

Out of scope:

- Flaws in a provider's own API (tell the provider)
- Social engineering
- Denial of service

## Supported Versions

Security fixes go into the latest release only.
