# hushrun (hushrun)

Installs [hushrun](https://hushrun.com), which skips a check this exact code already passed, and,
when the container is created, fetches what the repository's backend has for it.

```json
"features": {
  "ghcr.io/hushrun/features/hushrun:1": {}
}
```

## Options

| Option | Type | Default | What |
|---|---|---|---|
| `version` | string | `latest` | The release to install, such as `0.20.0`. |
| `fetch` | boolean | `true` | On create, `hushrun fetch`: the newest result of each check, and compiled code. |
| `pull` | boolean | `false` | On create, `hushrun build pull`: the repository's newest baked store, into `/hushrun`. |

## A credential

The feature holds none. To share results with your team, give the container its own: a
`HUSHRUN_AGENT` secret, or a bearer token as `HUSHRUN_TOKEN` with `HUSHRUN_AUTH=bearer`. In
GitHub Codespaces, add it as a Codespaces secret. A person signs in once with `hushrun login`.

`hushrun doctor` in the container says which hosts hushrun needs and whether they answer.
