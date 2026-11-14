# Using API keys through AgentVault

[AgentVault](https://github.com/michaelbaker-dev/agentvault) is a separate program that
stores API keys and makes requests with them, so an agent can use a key without ever
seeing it. spandeck works without it. With it, each station holds one vault token and
agents call provider APIs through spandeckd.

## Connect a station

1. Run AgentVault on a machine in your tailnet, served over HTTPS on port 443.
2. In the Control Deck, open **Vault**. spandeck looks for the vault among your Tailscale
   machines. To name it yourself, add this to `~/.config/spandeck/config.toml`:
   ```toml
   [vault]
   url = "https://vault-machine.your-tailnet.ts.net"
   ```
3. Click **Connect** on a station. A small window opens on the vault's own page. Check
   the station name, approve, and the window closes. The vault passphrase is only ever
   typed there, never into the Control Deck.

The station now holds a token named `spandeck:<machine name>`. It is saved in spandeck's
state folder, readable only by you. It can list keys and send requests through the vault,
read-only (GET) unless you widen it at the vault, and only from that machine.

## Let a project use keys

Name the keys in the project file. `*` matches any run of characters.

```toml
# .spandeck/project.toml
[vault]
keys = ["OPENAI_API_KEY", "github-*"]
```

Agents in that project get three tools from spandeck's MCP server: `vault_list`,
`vault_describe` and `vault_call`. A call for a key the project does not name is refused
before the vault is asked. Every call is written to the ship's log with the agent, key
name, method, path and status, never the key.

## Rotation

Tokens do not expire unless you say so. In the Vault screen, set **Token rotation** for a
station to an interval of one hour or more; spandeckd then swaps its own token on time.
You are notified if the setting is changed anywhere but the Control Deck, or if the vault
replaces or refuses the token.

## Raw keys

While a station is connected:

- a command or file write from an agent that contains something shaped like a key is
  blocked (Claude Code and Codex hooks), and the value is reported to the vault;
- a key pasted into a prompt, or found later in a session transcript, is reported.

The vault decides whether a reported value is one of its keys and tells the owner.
Detection is by pattern, so it is best-effort.

## Disconnect

**Disconnect** makes the station forget its token. Revoke `spandeck:<machine name>` on
the vault's Tokens page to end it at the vault too.

## What this does not protect against

- An agent can still misuse a key it is allowed to call, within what the token permits.
- Anyone with your user account on a station can read the token file.
- A key an agent gets some other way (a `.env` file, your shell) is outside the vault.

## The contract spandeck relies on

| Call | Use |
| --- | --- |
| `GET /v1/status` | Find the vault: `service: "agentvault"`, `version`, `sealed` |
| `POST /v1/token-requests` | Ask for a token; returns `request_id`, `approve_url`, `collect_code` |
| `GET /v1/token-requests/{id}` with `X-Collect-Code` | Poll, then collect the token once; 404 after that |
| `GET /v1/tokens/self` | The token's own scopes and rotation setting |
| `PUT /v1/tokens/self/rotation`, `POST /v1/tokens/self/rotate` | Set the interval; swap the token |
| `GET /v1/secrets`, `GET /v1/secrets/{name}` | Key names and descriptions, never values |
| `/v1/proxy/{name}/…` | Send a request with the key attached by the vault |
| `POST /v1/attention/report` | Report a key seen in the open: `{value, place}` |

Errors carry a `code`: `token_replaced`, `token_rotated` (401) and `token_overdue` (403).
