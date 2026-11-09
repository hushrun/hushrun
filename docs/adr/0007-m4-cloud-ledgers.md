# 0007. Cloud ledgers: choices made in M4

- **Status:** superseded by [0022](0022-https-ledgers-only-and-no-kms.md): bucket ledgers are gone, and so
  are the KMS signers that used the credential chains of items 7 and 8
- **Date:** 2026-10-14
- **Rules:** L-1, L-3, L-4, L-6, L-7, L-9, T-4, §11.3, §12.3

## Context

M4 adds `gs://`, `s3://` and `az://` ledgers over `object_store`, a native GCS
`external_account` provider, a token-command escape hatch, `ledger gc`, and conformance runs on
real stores. The plan names the pieces but leaves several details open. Following plan §0.1,
each choice below runs more checks or deploys less, or keeps credentials safer.

## Decision

1. **TLS uses ring, and nothing else.** `object_store` is built from its `*-base` features plus
   `ring`, and reqwest and rustls without a crypto provider. hushrun installs ring's provider
   before the first client. `aws-lc-sys` never enters the build (`deny.toml` bans it).
2. **The backend keeps the `Ledger` trait synchronous** (ADR 0005): an `ObjectLedger` owns a
   four-thread tokio runtime, created only when a cloud ledger is opened, and blocks on it.
   `get_many` fetches with 16 requests in flight (§12.4).
3. **Retries (L-4).** `object_store` retries 429, 5xx and timeouts of idempotent requests, up
   to three times with jittered exponential backoff. It doesn't retry a timed-out conditional
   create, because it can't know that one is idempotent. L-3 makes ours idempotent, so
   `append` retries those timeouts itself: three attempts, then exit 6. Each request times out
   after 10 s. The timeouts are set as configuration keys, not with `with_client_options`,
   which would drop what each store reads from the environment (proxies, `AWS_ALLOW_HTTP`).
4. **The 60 s budget (L-4)** covers the ledger requests of `plan` and `status`. Time spent
   waiting by design (upstream grace, `--wait-timeout`) is outside any request, so it isn't
   counted.
5. **`list_first` streams.** GCS, S3 and Azure list keys in lexicographic order, so the
   backend stops after the first `n` matches instead of listing a whole deployed index. String
   prefixes that end mid-segment (L-3) list the enclosing directory and filter.
6. **`HUSHRUN_LEDGER_TOKEN_COMMAND` is the token command**, read only from the environment,
   like `HUSHRUN_LEDGER` (T-4): a repository must not choose a command that runs with ledger
   credentials. It runs through the shell, applies to `gs://` and `az://` (S3 signs with keys),
   and its token is reused for 5 minutes. Errors show its stderr, never its stdout.
7. **`external_account` is implemented natively** (about 250 lines plus tests), without
   `google-cloud-auth`, which brings its own HTTP and TLS stack. It reads the subject token
   from a file or URL, exchanges it at STS, optionally impersonates a service account, and
   caches the token until 5 minutes before it expires. It sends tokens only to
   `https://*.googleapis.com`, so a credentials file can't redirect them. Executable and
   AWS-sourced credentials aren't supported; the error points to the token command.
8. **AWS profiles: static keys only.** `object_store` reads the environment, web identity,
   ECS and IMDS, but not profiles. hushrun reads `AWS_PROFILE` from `~/.aws/credentials` and
   `~/.aws/config` (keys and region). Profiles that assume roles, use SSO or run a
   `credential_process` get a warning and fall through to instance credentials.
9. **Azure CLI credentials need `AZURE_USE_AZURE_CLI=true`** (`object_store`'s switch): the CLI
   is slow to start and must not be the default, like gcloud.
10. **GC works from keys.** An event key's ULID and a deployed-index key's inverted timestamp
    date it, by the signer's clock. GC keeps, per (scope, stage), both the newest verified
    deployed entry and the newest one accepted for the stage's events (the one the planner
    uses, P-10), and every event of their digests. Keys it can't parse are kept. Deletion is
    sequential and stops at the first error; running GC again continues.
11. **Conformance on real stores.** MinIO no longer publishes container images, so CI uses
    versitygw for S3, Azurite for Azure, and the fake-gcs-server build that `object_store`'s own
    CI uses for GCS. That emulator ignores conditional creates, so GCS gets every check except
    create-only in CI, and all of them nightly against a real bucket, through workload identity
    federation and item 7. Images are pinned by digest.
12. **The cloud backends are a cargo feature** (`cloud`) of `hushrun-ledger`, which the binary
    enables. Crates that only need the file and mem ledgers, such as the testkit, build without
    the network stack.

## Consequences

- S3 and Azure behavior is checked end to end against emulators on every pull request; GCS's
  create-only behavior only nightly, and only once the repository variables are set.
- The `plan` latency budget against GCS (§12.5) can only be measured from GitHub-hosted
  runners against a real bucket; it isn't measured yet.
- Supporting more credential sources later (AWS SSO, executable-sourced federation) means new
  providers behind the same `CredentialProvider` trait; nothing else changes.

## Alternatives considered

- **`google-cloud-auth`.** It covers `external_account`, but pulls in a second HTTP client and
  its own TLS configuration, and its crypto provider choice would have to be policed.
- **Reading the token command from `hushrun.yaml`.** A pull request could then run any command
  with the ledger's credentials. Rejected.
- **Building MinIO from source in CI.** Its Go module has no tagged versions, only untagged
  commits, and building a server per CI run is slow; versitygw publishes versioned images.
