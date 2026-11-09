# hushrun

hushrun skips a check this exact code already passed: on your laptop, in CI and in an agent's
sandbox. This package puts its `hushrun` command on your PATH through npm.

[Docs](https://hushrun.com/docs) · [Quickstart](https://hushrun.com/docs/quickstart) ·
[Compared with Nx, Turborepo and Bazel](https://hushrun.com/docs/compare) ·
[Pricing](https://hushrun.com/pricing)

## Set up a repository

At the repository's root:

```bash
npx hushrun init
```

`init` finds the checks in package.json's scripts and in CI's own files, in any language, signs
you in, shows the changes, and applies them when you say yes: each check runs through `hushrun
run`, and package.json gets hushrun in devDependencies, so every teammate and CI job runs the same
hushrun. In a repository with no package.json, CI's files get the check and the step that installs
hushrun, and hushrun.yaml says where results go.

```json
"scripts": {
  "test": "hushrun run -- vitest run"
}
```

Run a check twice, and the second run skips:

```text
$ npm test
hushrun: test running: no pass yet for this exact code
  … vitest's output …
hushrun: test passed in 41s; it won't run again on this code
$ npm test
hushrun: test skipped: this exact code passed just now (saves 41s)
```

A coding agent runs `npx hushrun init --yes`, which applies the changes without asking.

## How the package works

`hushrun` is a small Node script that runs the hushrun binary from the package for your
platform. That package is an optional dependency, so npm, pnpm, yarn and bun install only the
one that matches:

| Package | Platform |
|---|---|
| `@hushrun/cli-darwin-arm64` | macOS on Apple silicon |
| `@hushrun/cli-darwin-x64` | macOS on Intel |
| `@hushrun/cli-linux-arm64` | Linux on arm64 (static, any libc) |
| `@hushrun/cli-linux-x64` | Linux on x64 (static, any libc) |
| `@hushrun/cli-win32-arm64` | Windows on arm64 |
| `@hushrun/cli-win32-x64` | Windows on x64 |

Arguments, input and output pass through unchanged, and `hushrun` exits with the binary's exit
code, or by the same signal. It sets `HUSHRUN_NPM_PACKAGE` to `hushrun@<version>`, the package
`hushrun init` adds to devDependencies, unless it is already set.

An install that leaves out optional dependencies (`--omit=optional`, `--no-optional`) has no
binary, and `hushrun` says so. Where this package has no binary, `HUSHRUN_BINARY` names one to
run instead.

## Other installs

Without npm, on macOS and Linux:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh | sh
```

On Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.ps1 | iex"
```

## Update

At a terminal, hushrun says once a day when a newer release is out. Update this package with
your package manager, such as `npm install hushrun@latest`. `hushrun update` updates what the
installers above installed. To know, hushrun asks GitHub at most once a day, never in CI;
`HUSHRUN_NO_UPDATE_CHECK=1` turns that off.

## License

hushrun is open source under the MIT license:
[LICENSE.md](https://github.com/hushrun/hushrun/blob/main/LICENSE.md).
[THIRD-PARTY-NOTICES.md](https://github.com/hushrun/hushrun/blob/main/THIRD-PARTY-NOTICES.md)
lists the open-source crates it is built with, and their licenses. This package holds both files.
