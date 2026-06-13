# Audit log

`abox-proxyd` records every proxied request — both intercepted CLI commands and
HTTPS egress — to a hash-chained JSON-lines file at
`~/.abox/logs/audit.jsonl`. `abox audit` inspects and verifies it.

## Commands

```bash
abox audit show                      # last 20 entries
abox audit show -n 100               # last 100
abox audit show --sandbox fix-auth   # filter by sandbox
abox audit show --request-type egress
abox audit verify                    # verify the keyed hash chain + tip
```

`abox doctor` also runs the same verification under the "Audit Log" section.

## Format

Each line is a JSON object:

| field          | meaning                                              |
| -------------- | ---------------------------------------------------- |
| `seq`          | monotonically increasing sequence number             |
| `timestamp`    | ISO 8601 UTC                                          |
| `sandbox_id`   | the sandbox that made the request                    |
| `request_type` | `cli` or `egress`                                    |
| `target`       | the command or domain                                |
| `detail`       | full CLI args (or empty for egress)                  |
| `decision`     | `allowed` or `denied`                                |
| `result_code`  | exit code (CLI) or HTTP status (egress)              |
| `prev_hash`    | hash of the previous entry (or 64 zeros for seq 0)   |
| `hash`         | keyed HMAC over the previous hash and this entry     |

Alongside the log are two host-only files (mode `0600`):

- `audit.key` — the HMAC key used to chain the log.
- `audit.tip` — the latest `{seq, hash}`, used to detect truncation.

## Tamper-evidence — the threat model

The chain hash is a **keyed** HMAC-SHA256:

```
hash = HMAC_SHA256(key, "abox-audit-v1" || prev_hash || "||" || canonical_core)
```

What this **does** guarantee:

- A compromised **guest/agent cannot forge or rewrite the log.** The agent runs
  inside the microVM and has no access to the host log *or* the key.
- Accidental or naive edits, insertions, deletions, and re-orderings are
  detected by `abox audit verify`.
- **Truncation** of the log tail is detected by comparing the verified chain
  against the persisted tip in `audit.key`'s sibling `audit.tip`.

What it does **not** guarantee:

- It does not defend against an attacker who already holds the **host key**
  (e.g. root on the host machine). Such an attacker can recompute a valid chain.
  This is inherent to any single-host scheme.

For stronger guarantees against a fully-compromised host, periodically export
the chain tip (`seq` + `hash`) reported by `abox audit verify` to an append-only
or external sink, and compare against it later.

## Durability and concurrency

- Every entry is `fsync`'d before it is treated as committed.
- The writer holds an exclusive advisory lock (`flock`) on the log file, so two
  `abox-proxyd` processes cannot interleave writes and fork the chain.

## Verification semantics

`abox audit verify` walks the chain from `seq` 0 and stops at the **first**
structural failure (parse error, sequence gap, broken link, or hash mismatch),
reporting the exact line and `seq`. This pinpoints the tamper location instead
of emitting a cascade of downstream errors. A non-zero exit status indicates a
failed verification.
