# Security Policy

## Scope and current threat model (`v0.0.1`)

`tinybrowser` is an early-stage headless browser (`v0.0.1`). While it enforces process-level site isolation, browser-owned networking (CORS, ORB, 6265bis cookies + CHIPS, CSP), and loopback CSRF protections (`Origin` and `Sec-Fetch-*` rejection on the CDP/WebDriver control server), **full OS-level renderer sandboxing is not yet complete on all platforms**.

Treat untrusted web pages with the same caution you would apply to an unsandboxed parser/JS runtime. Run `tinybrowser` inside a container or VM when browsing untrusted origins in production.

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Instead, report vulnerabilities privately via [GitHub Security Advisories](https://github.com/tinybrowser-dev/tinybrowser/security/advisories/new) or email **erickchristian48@gmail.com** with:

- A minimal reproduction (HTML/JS snippet, CDP/WebDriver script, or test case)
- Affected version / commit SHA and OS/architecture
- Impact assessment (e.g., renderer escape, origin bypass, crash/DoS)

We will acknowledge reports within 72 hours and coordinate a fix before public disclosure.
