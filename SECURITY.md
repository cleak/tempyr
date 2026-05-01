# Security Policy

## Reporting Vulnerabilities

Please report suspected vulnerabilities privately by opening a GitHub security
advisory on this repository. If advisories are unavailable, contact the
maintainers through the repository owner profile and avoid posting exploit
details in a public issue.

Include:

- Affected version or commit.
- Impact and expected exposure.
- Steps to reproduce, if safe to share.
- Any suggested fix or mitigation.

## Secret Handling

Tempyr reads provider keys from the environment or local `.env` files. Do not
commit real API keys, tokens, private keys, or generated local configuration.
The repository includes fake secret-shaped strings in redaction tests; they are
allowlisted in `.gitleaks.toml` for scanner noise only and are not usable
credentials. GitHub's native secret scanning may still report these fixtures;
maintainers should dismiss them as test credentials only after verifying the
exact literal appears in the allowlist.
