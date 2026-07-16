# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.51.x  | ✅ Current stable line; security and correctness fixes |
| < 0.51  | ❌ No active maintenance promise |

The latest stable line receives security fixes for at least six months. Support
and MSRV are reviewed quarterly and are never changed silently. `0.51.x` is the
first candidate line under the current policy, so no earlier candidate line is
promised. The canonical policy is
[`VERSION_POLICY.md`](https://github.com/ranvier-rs/docs/blob/main/05_dev_plans/VERSION_POLICY.md).

## Reporting a Vulnerability

We take security issues seriously. If you discover a security vulnerability in Ranvier, please report it responsibly.

### How to Report

1. **DO NOT** open a public GitHub issue for security vulnerabilities.
2. Email **security@ranvier.studio** with:
   - A description of the vulnerability
   - Steps to reproduce
   - Potential impact assessment
   - Any suggested remediation (optional)

### What to Expect

| Stage | Timeline |
|-------|----------|
| Acknowledgment | Within **48 hours** |
| Initial assessment | Within **5 business days** |
| Critical fix target | Within **7 calendar days** |
| High fix target | Within **14 calendar days** |
| Public disclosure | After fix is released, or **90 days** max |

The primary release/security owner is **Sanghyo Lee** and the distinct backup
is **이시완**. If the primary is unavailable, the backup owns acknowledgment,
assessment, and the release go/no-go record. A High or Critical dependency
finding blocks publication even after it is triaged.

### Severity Classification

| Severity | Description | Response Time |
|----------|-------------|---------------|
| **Critical** | Remote code execution, data breach, auth bypass | Fix within 7 days |
| **High** | Privilege escalation, significant data exposure | Fix within 14 days |
| **Medium** | Limited impact, requires specific conditions | Fix within 30 days |
| **Low** | Minimal impact, defense-in-depth improvement | Next release cycle |

## CVE Process

1. Vulnerability confirmed → CVE ID requested via MITRE or GitHub Security Advisories
2. Fix developed and reviewed in private branch
3. Security advisory published on GitHub
4. Patched version released to crates.io
5. RustSec advisory submitted (if applicable)

## Security Best Practices for Ranvier Applications

See the [Security Hardening Guide](https://ranvier.studio/docs/security-hardening) for:
- OWASP Top 10 compliance patterns
- Production deployment security checklist
- Input validation and sanitization
- Security header configuration

## Automated Supply-Chain Gates

Maintainers install the exact reviewed scanner versions and run the shared
wrapper. The wrapper preserves RustSec JSON, validates any exception record,
and runs the license/source policy from `deny.toml`:

```bash
cargo install cargo-audit --version 0.22.2 --locked
cargo install cargo-deny --version 0.20.2 --locked
node scripts/supply_chain_gate.mjs --self-test
```

The gate runs on relevant pull requests and `main` pushes, every Monday at
03:17 UTC, and from the release bundle. The tracked
`security/advisory-triage.json` starts empty. Medium/Low/Unknown temporary
exceptions require primary and backup approval, rationale, mitigation, review,
and expiry; stale or expired records fail. High/Critical findings cannot be
temporarily allowed for publication.

Release provenance is generated from a clean exact-Rust-1.95 checkout:

```bash
node scripts/release_provenance.mjs --self-test
node scripts/release_provenance.mjs
```

It verifies and hashes all 12 publishable `.crate` files plus a source archive.
Local `provenance.json` is explicitly unsigned. Tag/manual CI bundles the
evidence and applies a GitHub artifact attestation; consumers should verify that
attestation and compare published registry bytes before relying on provenance.

## Acknowledgments

We gratefully acknowledge security researchers who responsibly disclose vulnerabilities. Contributors will be credited in security advisories (unless they prefer anonymity).
