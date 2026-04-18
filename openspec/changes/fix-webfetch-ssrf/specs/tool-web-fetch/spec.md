## ADDED Requirements

### Requirement: WebFetch SSRF Guard

The WebFetch tool SHALL refuse any request whose resolved destination IP
falls into a private, loopback, link-local, CGNAT, cloud-metadata, or
unique-local range, unless the environment variable
`CC_WEBFETCH_ALLOW_PRIVATE` is set to `1`.

The guard MUST run **before** any network socket is opened to the target
host. The resolved IP MUST then be used for the actual HTTP request
(pinned resolution) so that a subsequent DNS response cannot swap the
destination between the check and the fetch.

The same guard applies to `cc-tools::web_search` when it follows
individual result URLs.

#### Scenario: Public host passes the guard
- **WHEN** WebFetch is invoked with `https://example.com/`
- **THEN** the request proceeds and the body is returned to the caller

#### Scenario: Cloud metadata endpoint is refused
- **WHEN** WebFetch is invoked with `http://169.254.169.254/latest/meta-data/`
- **THEN** the tool returns a `ToolResult::error` whose message names the
  refused host
- **AND** no TCP connection is attempted (verified by a test middleware)

#### Scenario: Loopback is refused
- **WHEN** WebFetch is invoked with `http://localhost:6379/` or
  `http://127.0.0.1/admin`
- **THEN** the tool returns `ToolResult::error` referencing loopback

#### Scenario: RFC1918 / link-local / ULA are refused
- **WHEN** WebFetch is invoked with a URL resolving to `10.*`, `172.16–31.*`,
  `192.168.*`, `169.254.*` (other than the metadata IP, which is its own
  case), `[fe80::/10]`, or `[fc00::/7]`
- **THEN** the tool returns `ToolResult::error` naming the refused IP

#### Scenario: Developer opt-out
- **GIVEN** `CC_WEBFETCH_ALLOW_PRIVATE=1` is set in the process environment
- **WHEN** WebFetch is invoked with a private-range URL
- **THEN** the fetch proceeds as normal and a warning is logged at `warn!`

#### Scenario: DNS rebinding is blocked
- **GIVEN** the host resolves to a public IP at check time
- **WHEN** the DNS response changes to a private IP before the HTTP socket
  is opened
- **THEN** the tool still connects to the IP pinned at check time, not the
  rebind target
