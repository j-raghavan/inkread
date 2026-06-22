# Security Policy

## Supported versions

inkread is pre-1.0 and ships from `master`. Security fixes target the **latest release** and
`master`; older tagged builds are not patched in place — please upgrade.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately via GitHub's [**Report a vulnerability**](https://github.com/j-raghavan/inkread/security/advisories/new)
(Security → Advisories) so we can triage and fix before disclosure. If you can't use that, email
**jrlabs01@gmail.com** with `inkread security` in the subject.

Please include:

- the affected component (e.g. the PDF/EPUB parser, the JNI bridge, the Lua plugin runtime, the
  dictionary/SQLite path);
- a description and impact;
- a minimal reproduction — a crafted document or input is ideal. **Do not** attach private research
  material (decompiled APKs, `.so`, jadx output); a self-contained fixture is enough.

We aim to acknowledge within **72 hours** and to agree on a disclosure timeline with you. We're happy
to credit you in the advisory unless you'd prefer to stay anonymous.

## Where the risk lives

inkread parses **untrusted documents** and runs **untrusted Lua plugins**, so the areas of most
interest are:

- document parsing & rendering (`inkread-pdftext`/pdfium, `inkread-epub`) — malformed/malicious files;
- the **JNI boundary** — input is validated at the boundary and panics are caught and converted to
  Java exceptions rather than crossing JNI (RR21-FR3); a bypass is in scope;
- the embedded **Lua** plugin runtime (`inkread-lua`) — sandbox escape or resource exhaustion;
- the bundled **SQLite** dictionary path.

## Good to know

- The Rust core is memory-safe Rust and builds host-only, which makes most parser bugs reproducible
  off-device with a fixture.
- Dependencies are license-gated (`scripts/check-licenses.sh`); we also welcome reports of vulnerable
  dependency versions.
