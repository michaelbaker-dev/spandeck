# Using API keys through AgentVault

[AgentVault](https://github.com/michaelbaker-dev/agentvault) is a separate program that
stores API keys and makes requests with them, so an agent can use a key without ever
seeing it. spandeck works without it. With it, each station holds one vault token and
agents call provider APIs through spandeckd.

## Connect a station

1. Run AgentVault on a machine in your tailnet, served over HTTPS on port 443.
2. In the Control Deck, open **Vault**. spandeck looks for the vault among the machines
   signed in to your own Tailscale login. A vault on a tagged machine is not searched
   for; name it yourself in `~/.config/spandeck/config.toml`:
   ```toml
   [vault]
   url = "https://vault-machine.your-tailnet.ts.net"
   ```
3. Click **Connect** on a station. A small window opens on the vault's own page. Check
   the station name, approve, and the window closes. The vault passphrase is only ever
   typed there, never into the Control Deck.

A Control Deck that opens on a station that found a vault and is not connected offers to
connect it, once, in a bar at the top. **Not now** is remembered by the station; the Vault
screen has **Remind me again**.

Once connected, the station talks to the vault that issued its token. If the vault moves
to another address, the station looks for it among your machines when the old address
stops answering, follows the one that still accepts its token, and tells you it did.

The station now holds a token named `spandeck:<machine name>`. It is saved in spandeck's
state folder, readable only by you. It can list keys and send requests through the vault,
read-only (GET) unless you widen it at the vault, and only from that machine.

## What agents can use

Nothing more to set up. Once a station is connected, every agent spandeck starts on it
(Claude Code or Codex) has three tools from spandeck's MCP server: `vault_list`,
`vault_describe` and `vault_call`. By default an agent may use every key you allowed
that station's token at the vault. Every call is written to the ship's log with the
agent, key name, method, path and status, never the key.

To narrow a project's agents to some keys, name them in the project file. `*` matches
any run of characters. A call for a key the project does not name is refused before the
vault is asked.

```toml
# .spandeck/project.toml
[vault]
keys = ["OPENAI_API_KEY", "github-*"]
```

To narrow a whole station, limit its token at the vault.

## Checks

spandeckd checks the whole chain when it starts, when a Control Deck opens and when you
press **Check again** on the Vault screen. The screen lists each check per station;
`spandeck doctor` has a `vault` line; `spandeck rpc vault.check` runs it from a terminal.

| Check | What is looked at | Repaired without you |
| --- | --- | --- |
| Vault address | AgentVault answers at the address the station has | A vault that moved is found again |
| Vault unlocked | The vault says it is unlocked | No: unlock it on the vault's page |
| Station token | The vault accepts the token, and has it tied to a machine | A token overdue for rotation is swapped. A revoked one is not: **Reconnect**, and approve at the vault |
| Keys for agents | The token can list keys, as an agent's `vault_list` does | No |
| Agent panes | spandeck's program and socket, which every agent's vault tools use; Antigravity's plugin | Antigravity's plugin is rewritten |

A problem found is put in your inbox once, not on every check.

To turn the integration off on a station, or to add addresses to the search:

```toml
[vault]
enabled = false                      # no vault is looked for or mentioned
search = ["https://host.example:8443"]   # asked as well as your Tailscale machines
```

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
| `GET /v1/tokens/self` | The token's own scopes, rotation setting and `nodes` (the machines it is tied to) |
| `PUT /v1/tokens/self/rotation`, `POST /v1/tokens/self/rotate` | Set the interval; swap the token |
| `GET /v1/secrets`, `GET /v1/secrets/{name}` | Key names and descriptions, never values |
| `/v1/proxy/{name}/…` | Send a request with the key attached by the vault |
| `POST /v1/attention/report` | Report a key seen in the open: `{value, place}` |

Errors carry a `code`: `token_replaced`, `token_rotated` (401) and `token_overdue` (403).
