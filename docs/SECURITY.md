# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.13.x  | ✅ Current |
| 0.12.x  | ⚠️ Security fixes only |
| < 0.12  | ❌ End of life |

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
| Fix development | Within **30 days** for Critical/High |
| Public disclosure | After fix is released, or **90 days** max |

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

See the [Security Hardening Guide](docs/03_guides/security_hardening_guide.md) for:
- OWASP Top 10 compliance patterns
- Production deployment security checklist
- Input validation and sanitization
- Security header configuration

## Automated Security Scanning

We recommend the following tools for Ranvier applications:

```bash
# Dependency vulnerability scanning
cargo install cargo-audit
cargo audit

# Supply chain security
cargo install cargo-vet
cargo vet
```

## Acknowledgments

We gratefully acknowledge security researchers who responsibly disclose vulnerabilities. Contributors will be credited in security advisories (unless they prefer anonymity).
