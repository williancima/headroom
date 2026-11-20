# Adding a provider

A provider is a `ProviderKind` variant, a module in `headroom-core` with a client, a sign-in, and a mapper,
and a branch in `refresh.rs`. Every value it shows must come from the provider's API. Never read another
tool's credentials, files, or logs, and never compute spend from logs or pricing.

## 1. Declare the kind

In `crates/headroom-core/src/models.rs`, add the variant to `ProviderKind`, to `ALL`, and to `id()` and
`display_name()`. `ALL` is only the list of kinds; it never orders accounts.

## 2. Create the module

Add `crates/headroom-core/src/<name>/` and `pub mod <name>;` in `lib.rs`. Copy the layout of a small
provider such as `amp/`:

- `mod.rs`: `<Name>Config` (endpoints, injectable for tests; `Default` is the real service) and
  `<Name>Client` with `fetch(...)`. It sends the requests through a `reqwest::Client` and returns
  `ProviderError` on failure. `ProviderError` has a friendly message for each case; never log a credential or
  a response body.
- `mapper.rs`: a pure function from the response body to metric lines. Validate the response here.
- `login.rs` (not for API-key providers): the sign-in, written with the helpers in `oauth/`
  (`authorize_url` and `LoopbackServer` for a browser sign-in, `device_authorization` for a device code).
  Send a UUID as the OAuth `state`, never the PKCE verifier, and encode spaces as `%20`.

An API-key provider has no `login.rs`; `login::add_api_key` checks the key with one usage read.

## 3. Wire it up

- `refresh.rs`: add the kind to `fetch_inner` (API-key providers) or `fetch_oauth` (OAuth providers). A
  provider that keeps an hourly secondary read uses `ProviderState` (`last_secondary_read`, `cached_body`).
  Renewed tokens are written back to `OAuthTokens` in place; `fetch_inner` saves them.
- `login.rs`: add the kind to `is_oauth_kind` and `sign_in`, or to `add_api_key`.
- `envelope.rs`: add its `descriptors` if `headroom usage` should export its metrics.
- `priority/meter.rs`: only if its "Session" line needs its own rule for when a window has started.
- `crates/headroom-cli/src/main.rs`: add the name to the `--help` text and the "supported" message. The Add
  Account screen lists every kind in `ProviderKind::ALL`, so the panel needs no change.

## 4. Defaults

- Every metric with data shows. Confirm the metric order with the owner before you choose it: it is the
  order of the lines the mapper returns.
- A balance with no limit is a `Values` line, not a `Progress` one. It counts as used up at zero only when
  it is money or credits the user holds.

## 5. Test

- Mapper: `mapper_tests.rs` (or a `#[cfg(test)]` module) against recorded responses: success, empty plan,
  error.
- Client and sign-in: `tests.rs`, using `wiremock` with the config's URLs pointed at the mock server.
- Run `cargo test -p headroom-core <name>`.

## 6. Document

- `docs/providers.md`: a table row and a section.
- `docs/cli.md`: the kind and its resource keys.
- `README.md`: the provider list.
