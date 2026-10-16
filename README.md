# strata-configurator

Configuration model, compiler and read-only UI backend for strata.
Service `configurator-api` listens on port 8097 (`GET /health`).
Spec and plans: `docs/`.
Rust workspace: `configurator-core` (model/compiler), `configurator-api` (axum).
