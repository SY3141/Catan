# HexFish



## Running the web UI with Docker

The Docker image serves the 1v1 Catan web analysis board described
below, using the built-in `rollout` evaluator and exposing the board on
port 3000:

Rebuild the image so the Rust server changes are included:

```
docker compose up --build
```

The server chooses its port in this order: an explicit `--port`, the
`PORT` environment variable, then local default `3000`. This keeps
`docker compose` available at <http://localhost:3000> while allowing
Cloud Run to inject its required port.

WebSocket sessions use a browser-local anonymous session id. The frontend
can still require Clerk sign-in before showing the app, but the server no
longer validates Clerk JWTs or depends on Clerk server environment variables.

Replay logs are stored in Postgres when `DATABASE_URL` is set. The
included `docker-compose.yml` starts a local Postgres service and wires
`DATABASE_URL` automatically; without `DATABASE_URL`, the server falls
back to local replay log files.

Live multiplayer rooms use in-memory state by default. For shared
multiplayer state across multiple server instances, set
`HEXFISH_MULTIPLAYER_BACKEND=redis` and `REDIS_URL=redis://HOST:6379`.
The included `docker-compose.yml` starts Redis and enables that backend
for local container testing. In production, set
`HEXFISH_MULTIPLAYER_REQUIRED=true` so startup fails clearly if Redis is
not reachable.

For production, set `HEXFISH_REPLAY_STORE_REQUIRED=true` so the server
fails startup if Postgres replay storage cannot connect. Leave it unset
or `false` for local development so filesystem replay logs remain a
fallback.

If you need to wipe old Postgres replays after a replay-schema change,
take any backup you want first, then run:

```sql
TRUNCATE TABLE replay_log_accounts, replay_logs RESTART IDENTITY CASCADE;
```

For the local Docker database, the same wipe can be run with:

```bash
docker compose exec postgres psql -U hexfish -d hexfish -c "TRUNCATE TABLE replay_log_accounts, replay_logs RESTART IDENTITY CASCADE;"
```

Then open <http://localhost:3000>.
