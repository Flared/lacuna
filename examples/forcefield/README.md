# Lacuna + ForceField

This example shows how to chain Lacuna with the
[ForceField LLM Security Gateway](https://github.com/forcefield-ai/force_field_llm_security_gateway)
to combine Lacuna's tailnet-native access control with ForceField's
prompt-injection / PII / secret-leak detection.

## Why chain them

Lacuna and ForceField solve different problems and sit cleanly at different
layers of the request path:

| Layer            | Concern                                                  | Tool       |
|------------------|----------------------------------------------------------|------------|
| Identity / ACL   | Which tailnet user may call which provider/model         | Lacuna     |
| Content security | Is this prompt a jailbreak? Does the response leak PII?  | ForceField |
| Provider routing | Where does the request actually go (OpenAI/Anthropic/.)  | Lacuna     |

Composing the two lets you keep Lacuna's zero-config Tailscale identity model
while gaining detection, redaction, and audit capabilities that Lacuna
intentionally does not implement.

## Request flow

```
  tailnet user
       |
       v
  +---------+   Tailscale-User-Login              x-api-key:     <ff-key>
  | Lacuna  | --------------------------------+   Authorization: Bearer <provider-key>
  +---------+                                 |   POST /v1/chat/completions
       |  (capability check, routing,         |   POST /v1/messages
       |   per-user metrics)                  v
       |                                +-------------+
       |                                | ForceField  |
       |                                | gateway     |
       |                                +-------------+
       |                                  | (auth via x-api-key,
       |                                  |  scan + redact, then strip
       |                                  |  FF auth and forward the
       |                                  |  bearer untouched)
       |                                  v
       |                                +---------+
       +------------------------------> | OpenAI  |
                                        | / Anthropic
                                        +---------+
```

Lacuna is configured with ForceField as a custom OpenAI-compatible upstream
*and* an Anthropic-native upstream. For both providers Lacuna sends two
independent headers:

- `x-api-key: ${FORCEFIELD_API_KEY}` -- authenticates the deployment to FF.
- `Authorization: Bearer ${OPENAI_API_KEY}` (or `${ANTHROPIC_API_KEY}`) --
  the user-owned upstream key. ForceField's `/v1/chat/completions` and
  `/v1/messages` handlers strip their own auth headers and forward the
  bearer straight through to api.openai.com / api.anthropic.com.

The provider bill goes to whoever owns the upstream key, not to ForceField.

## Files

| File                         | Purpose                                              |
|------------------------------|------------------------------------------------------|
| `lacuna-config.json`         | Lacuna config: ForceField as OpenAI-compat provider  |
| `docker-compose.yml`         | Runs Lacuna locally; expects ForceField on the host  |
| `.env.example`               | Environment variables (copy to `.env`)               |
| `claude_code.settings.json`  | Claude Code config pointing Anthropic SDK at Lacuna  |
| `smoke_test.py`              | End-to-end test: benign prompt + jailbreak attempt   |
| `gcp-cloud-run/`             | Variant pointing Lacuna at a hosted ForceField       |

## Quick start (local)

1. Run ForceField locally, following the
   [ForceField README](https://github.com/forcefield-ai/force_field_llm_security_gateway#quick-start).
   The edge-proxy must be reachable at `http://host.docker.internal:8080`
   (or change `lacuna-config.json`).

2. Provision a ForceField API key for your tenant (POST `/v1/onboard` on
   the tenant manager, or use the dashboard) and put it -- plus your real
   upstream provider keys -- in `.env`:

   ```
   cp .env.example .env
   # edit .env: FORCEFIELD_API_KEY, OPENAI_API_KEY, ANTHROPIC_API_KEY
   ```

3. Start Lacuna:

   ```
   docker compose up
   ```

4. Send a request through the chain:

   ```
   ./smoke_test.py
   ```

   The benign prompt should return a model completion. The jailbreak prompt
   should be blocked by ForceField with a non-2xx response or a refusal
   message in the `forcefield_metadata` field.

## Quick start (hosted ForceField on GCP Cloud Run)

See [gcp-cloud-run/README.md](gcp-cloud-run/README.md). The only difference
is the `baseurl` in the Lacuna config and that the ForceField key lives in
your secrets store of choice.

## Notes & caveats

- **OpenAI and Anthropic are both BYOK.** ForceField's gateway exposes
  `/v1/chat/completions` (OpenAI-compatible, BYOK by default) and
  `/v1/messages` (Anthropic-native passthrough with SSE streaming and
  inline PII redaction). The Lacuna config in this example declares both
  providers and routes by URL prefix: `/forcefield-openai/v1/chat/...`
  and `/forcefield-anthropic/v1/messages`.
- **ForceField never holds your provider key.** Lacuna sends
  `Authorization: Bearer ${OPENAI_API_KEY}` (or `${ANTHROPIC_API_KEY}`)
  alongside `x-api-key: ${FORCEFIELD_API_KEY}`. The gateway reads its own
  key off `x-api-key`, strips the FF auth headers, and forwards the bearer
  untouched to api.openai.com / api.anthropic.com. The provider bill goes
  to the key owner, not to ForceField. ForceField's security pipeline
  (detectors, PII redaction, output moderation) runs on local models -- no
  external LLM cost.
- Bedrock and Gemini ingress on ForceField are not yet implemented.
- **Two-header auth, not one.** Lacuna 0.20.x sets exactly one auth header
  per provider via `authorization`. To send both the FF tenant key *and*
  the upstream provider key, this example uses `authorization: bearer`
  with `apikey: ${PROVIDER_KEY}` and pins
  `headers: { "x-api-key": "${FORCEFIELD_API_KEY}" }` as a static header.
- **Model-glob enforcement is provider-dependent.** Lacuna only extracts
  the request `model` for Anthropic / Bedrock / Gemini. The OpenAI
  chat-completion and responses handlers do not inspect the body, so a
  `models: ["gpt-*"]` capability would deny every request. This example
  uses `models: ["*"]` for the OpenAI provider; rely on Lacuna's
  `capabilities_header` (Tailscale grants) or downstream FF policy to
  restrict OpenAI models.
- **Capabilities header is opt-in here.** The default config does not set
  `capabilities_header`. Lacuna treats a missing capability header as
  `deny_all`, so leaving it on would block every request from clients
  that aren't passing through Tailscale grants. Re-enable it (and pass
  `Tailscale-App-Capability`) once you are running Lacuna inside a real
  tailnet.
- **User attribution.** Lacuna injects `Tailscale-User-Login` upstream
  when `identity_header` is set. ForceField records it in its audit trail.
- **Tailscale trust mode is self-host only.** The hosted Cloud Run gateway
  cannot use it because Lacuna's egress IP to Cloud Run is not in the
  tailnet CGNAT range. Use the `x-api-key` path for SaaS deployments;
  Tailscale trust is for ForceField instances co-located with Lacuna in
  the tailnet.
- **Defence in depth.** Lacuna's capability filter runs *first*, so
  requests blocked at the ACL layer never hit ForceField -- saving
  inference cost. Requests that pass Lacuna are then scanned by FF's
  detectors and postprocessor.
