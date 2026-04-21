# Vercel / Context.ai Breach: Ein Architecture Analysis

_Date: 2026-04-20_

---

## What Happened

The breach was a double supply chain attack:

1. An infostealer compromised a Context.ai employee, giving attackers access to Context.ai's internal systems and its OAuth application credentials.
2. At least one Vercel employee had installed Context.ai and authorized it with broad OAuth scopes against their corporate Google Workspace account.
3. The MCP server architecture used by Context.ai forwarded the employee's OAuth tokens on every agent interaction. With Context.ai's server compromised, the attacker silently exfiltrated those tokens — no anomalous API calls, no audit log entries from the victim's side.
4. Those tokens were used to access Vercel's Google Workspace, pivot to internal Vercel systems, and read environment variables that were not marked "sensitive."

The MCP-specific design issue that made step 3 so clean: a compliant MCP server receives every piece of context passed to it — including auth tokens — and the protocol has no mechanism to prevent a server from silently forwarding that data outbound before returning a result. There is no sandbox, no network policy, no audit hook.

---

## Where Ein's WASM Architecture Would Have Helped

### 1. Outbound network allowlisting

Every plugin's outbound HTTP is gated by `allowed_hosts` in `SessionConfig`. Each plugin gets its own allowlist, enforced in `ModelClientHarnessState::send_request` before any packet leaves the process. A compromised or malicious MCP tool could not exfiltrate tokens to an attacker-controlled host unless that host was in the allowlist.

The Context.ai attack succeeded precisely because the MCP server could call home freely. In Ein's model, you must explicitly allow a destination host before any data can reach it. The blast radius of a compromised tool is bounded by what you authorized it to call.

### 2. No ambient authority — deny-by-default capabilities

The WASM sandbox gives each tool only what is explicitly preopened:
- **Filesystem:** only paths in `WasiCtxBuilder::preopened_dir`. No access to `~/.ssh`, browser cookie stores, credential managers, or any other path the user did not grant.
- **Network:** only `allowed_hosts`. No unrestricted socket access.
- **Environment:** no access to the host process's environment variables. In the Vercel incident, unmasked env vars were pivotal — a WASM tool in Ein cannot read the host process environment at all.

A WASM tool receives only what the agent loop explicitly passes to it as `args`. It has no side channel to ambient credentials.

### 3. Per-plugin config isolation

Each plugin's credentials and capabilities are scoped in `plugin_configs` and cannot leak across plugins. A compromised tool cannot read credentials that were passed to a different plugin.

### 4. Auditable tool call surface

`AgentEvent::ToolCallStart` and `ToolCallEnd` are emitted for every call, including the raw `arguments` and `result`. This is not tamper-evident logging, but the shape is there to build on. In the Context.ai incident there were "no anomalous API calls from the victim's perspective" — Ein's events at least create a local record of what each tool was invoked with.

---

## Where Ein's Architecture Would Not Have Helped

**The infostealer on Context.ai's employee.** Endpoint security problem, entirely outside Ein's scope.

**OAuth scope over-permission.** The employee authorized "Allow All" scopes. Ein has no opinion on how OAuth is configured between the user's identity provider and the app they connect to.

**Broad `allowed_hosts` configurations.** If the user configures `allowed_hosts = ["*"]` for a plugin — or uses `ein_bash` with unrestricted network access — the protection collapses. A Bash tool with `*` as its allowlist can `curl` tokens anywhere. The architecture creates the right *shape* for security but cannot enforce that users configure it narrowly.

**The MCP protocol design issue itself.** Ein's `ToolSet` abstraction has the same latent problem: a tool receives whatever the agent passes it, and nothing in the protocol prevents a tool from treating its inputs as data to exfiltrate. The allowlist is the only enforcement point, and it is coarse (hostname-level, not content-level).

---

## Summary

| Attack step | Ein mitigation | Strength |
|---|---|---|
| Malicious tool calls home with stolen token | `allowed_hosts` per-plugin allowlist | Strong, if configured narrowly |
| Tool reads ambient env vars / credentials | WASM has no env access | Strong unconditionally |
| Tool reads arbitrary filesystem paths | `preopened_dir` sandboxing | Strong, if configured narrowly |
| Credentials leak across plugins | Per-plugin `plugin_configs` isolation | Strong unconditionally |
| No audit trail of what tool received | `ToolCallStart`/`ToolCallEnd` events | Partial — exists but not tamper-evident |
| Infostealer on upstream vendor employee | None | Not applicable |
| Over-broad OAuth granted by user | None | Not applicable |

The architecture's strongest property is the combination of deny-by-default network access and no ambient authority — which directly addresses the two mechanisms the Vercel attacker exploited. The weakness is that these protections are only as good as the `allowed_hosts` configuration the user provides.

---

## Sources

- [Vercel Breach Tied to Context AI Hack Exposes Limited Customer Credentials](https://thehackernews.com/2026/04/vercel-breach-tied-to-context-ai-hack.html)
- [The Vercel April 2026 Incident: How a Compromised AI Integration Became a Supply Chain Attack](https://www.shipsafecli.com/blog/vercel-april-2026-ai-integration-supply-chain-attack)
- [Context.ai OAuth Token Compromise](https://www.wiz.io/blog/contextai-oauth-token-compromise)
- [The Vercel Breach: OAuth Supply Chain Attack](https://www.trendmicro.com/en_us/research/26/d/vercel-breach-oauth-supply-chain.html)
- [A Timeline of Model Context Protocol (MCP) Security Breaches](https://authzed.com/blog/timeline-mcp-breaches)
- [MCP Server Security: The Hidden AI Attack Surface](https://www.praetorian.com/blog/mcp-server-security-the-hidden-ai-attack-surface/)
- [Vercel April 2026 security incident](https://vercel.com/kb/bulletin/vercel-april-2026-security-incident)
