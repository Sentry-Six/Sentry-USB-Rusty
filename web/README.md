# Sentry USB Web Frontend

The Sentry USB web interface — a React single-page application with a dark glassmorphism design.

## Tech Stack

- **React 19** + TypeScript
- **Vite** — build tooling and dev server
- **TailwindCSS** — utility-first styling
- **Material Symbols** — icons, vendored as inline SVG in `src/components/icons.tsx`
- **Leaflet** — drive map visualization

## Development

```bash
npm install
npm run dev     # Starts dev server on http://localhost:5173
```

The dev server proxies `/api/*` (including WebSockets) and `/TeslaCam` to the Rust backend at `localhost:8788`. On a Linux dev box, start the backend in dev mode (it listens on 8788 by default and skips the embedded static files):

```bash
cd ..
mkdir -p crates/sentryusb/static   # the binary embeds this folder; it must exist to compile
cargo run -p sentryusb -- --dev
```

Building the backend on Linux also needs `libdbus-1-dev` and `pkg-config` for Bluetooth support.

To develop against a real device instead, point the proxy at it with `SENTRYUSB_API`, e.g. `SENTRYUSB_API=http://sentryusb.local npm run dev`. Set `SENTRYUSB_READ_ONLY=1` to block API requests other than GET, HEAD and OPTIONS from the dev server.

## Icons

Icons are Google Material Symbols, vendored as inline SVG in
`src/components/icons.tsx` so the app ships no icon font and makes no external
request. That file is generated — don't edit it by hand.

To add or remove one, put its name (exactly as shown on
[fonts.google.com/icons](https://fonts.google.com/icons)) in
`scripts/icons/symbols.mjs`, then regenerate:

```bash
npm run icons             # rewrites src/components/icons.tsx
npm run icons -- --check  # CI: fails if the committed file is stale
```

Glyphs come from the exact `@material-symbols/svg-400` version in `package.json`
and `package-lock.json`, installed by `npm ci`. Generation and checks run offline;
they do not fetch from a CDN. Review generated icon changes when updating the package.
Two legacy expand glyphs are preserved in `scripts/icons/overrides` to retain their
existing artwork because the pinned package does not include them.

Each symbol becomes a component named after it (`delete` → `DeleteIcon`), taking
the same props as any `<svg>`, so Tailwind sizing (`h-4 w-4`) works as usual.

## Production Build

```bash
npm run build   # Outputs to dist/
```

The built files are embedded into the `sentryusb` binary via `rust_embed` from `crates/sentryusb/static/`. After building the frontend:

```bash
cd ..
rm -rf crates/sentryusb/static && cp -r web/dist crates/sentryusb/static
cross build --release --target aarch64-unknown-linux-gnu -p sentryusb
```

See [BUILD.md](../BUILD.md) for the full build.

## Pages

| Page | Description |
|------|-------------|
| **Dashboard** | System status, CPU temp, WiFi, disk space, snapshots, drive map |
| **Viewer** | Multi-camera clip viewer with synced playback (6 cameras) |
| **Drives** | Drive history, per-drive detail, FSD analytics, and safety score estimate |
| **Charging** | Charge session history and per-session detail |
| **Files** | Browse/upload/delete TeslaCam, Lock Sounds, Wraps, License Plates, Music, LightShow, Boombox, and USB Drive files |
| **Snapshots** | Saved snapshot management |
| **Notifications** | Notification history, event toggles, and delivery providers |
| **Logs** | Live-tailing of archiveloop, setup, diagnostics, and Bluetooth logs |
| **Community** | Community wraps and lock chimes |
| **Support** | AI Support & Help chat |
| **Terminal** | In-browser shell |
| **Settings** | Setup Wizard, quick actions, system update, reboot |

## Structure

```
src/
├── api/               # Typed API clients (drives, charging, support, tags)
├── components/
│   ├── layout/        # AppShell, Sidebar, MobileNav, ConnectionBanner
│   ├── setup/         # SetupWizard + 11 step components
│   └── ...            # Feature components (drives, charging, viewer, settings, ...)
├── hooks/             # React hooks
├── pages/             # One component per route (see table above)
├── types/             # Shared TypeScript types
└── lib/               # API client, WebSocket hook, utilities
```
