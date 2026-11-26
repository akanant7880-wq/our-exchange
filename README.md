# our-exchange

Independent simulated cryptocurrency derivatives exchange (starting with **BTCUSDT Perpetual**).

## Architecture
- `apps/web`: Next.js + TypeScript web trading interface
- `apps/android`: React Native Android trading app
- `services/api`: Node.js + TypeScript backend API & WebSocket server
- `services/matching-engine`: Rust deterministic order-matching engine (fixed-point integer arithmetic)
- `packages/shared-contracts`: Shared data schemas and contract definitions
- `infra`: Deployment and database configurations
- `docs`: Architecture, accounting rules, and compliance notes
