# Lacuna + hosted ForceField (GCP Cloud Run)

Variant of the parent example that points Lacuna at a ForceField gateway
deployed on Google Cloud Run.

The reference instance is at:

```
https://forcefield-gateway-546798516374.northamerica-northeast1.run.app
```

It exposes both `/v1/chat/completions` (OpenAI-compatible BYOK) and
`/v1/messages` (Anthropic-native BYOK with SSE streaming and inline PII
redaction).

## Setup

1. Get a ForceField tenant key. Either ask Force Field to provision one
   for you, or POST to the tenant manager directly:

   ```sh
   curl -X POST https://forcefield-tenant-manager-dhowwtjefa-nn.a.run.app/v1/onboard \
     -H 'content-type: application/json' \
     -d '{"name":"your-team","contact_email":"you@example.com","plan":"professional"}'
   ```

   The response includes a `raw_key` that starts with `ffgw_...` -- that
   is your `FORCEFIELD_API_KEY`.

2. Set environment variables before launching Lacuna:

   ```sh
   export FORCEFIELD_BASE_URL=https://forcefield-gateway-546798516374.northamerica-northeast1.run.app
   export FORCEFIELD_API_KEY=ffgw_...
   export OPENAI_API_KEY=sk-...
   export ANTHROPIC_API_KEY=sk-ant-...
   ```

3. Start Lacuna with this directory's config:

   ```sh
   lacuna --config gcp-cloud-run/lacuna-config.json
   ```

   Or via Docker, mounting the config and forwarding the env vars.

## Cloud Run notes

- Cloud Run terminates TLS for you -- the `baseurl` must be `https://`.
- Cloud Run idle-scales to zero. The first request after a cold period
  incurs the gateway's startup latency (~1-3s for the FastAPI app, longer
  if detector models cold-start). Set `--min-instances=1` on the gateway
  service if cold starts are unacceptable.
- IAM auth is independent from FF tenant auth. The reference instance
  above is publicly reachable; the `x-api-key` header is the only auth
  layer between you and your tenant. If your gateway requires GCP IAM,
  put it behind an internal load balancer and run Lacuna in the same
  VPC connector with a service-account identity token.
- **Tailscale trust mode is not viable here.** Lacuna's egress IP to
  Cloud Run is not in the tailnet CGNAT range, so `TAILSCALE_TRUST_ENABLED`
  on the gateway will reject every request. Tailscale trust is for
  self-hosted ForceField sitting inside the same tailnet as Lacuna.
