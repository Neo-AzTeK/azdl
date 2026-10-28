# Browser game-control regression

Run `npm run dev -- --host 127.0.0.1 --port 5188` from `frontend/`, then open:

`http://127.0.0.1:5188/tests/game-controls.html`

The page mounts the real game page, shared controls and installed `m3-svelte` switch component. A local in-memory IPC fixture controls command completion and failures. It changes no game files and executes no real patch/runtime command.

The result reports mouse, keyboard and drag behavior, duplicate-command prevention, all-switch locking, confirmed state after success/failure, Windows disabled-overlay availability, runtime section placement and stale responses after switching games. These fixtures are not included in the production Vite entry point.
