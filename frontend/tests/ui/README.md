# Game page browser regression

Run `npm run dev` from `frontend`, open
`http://localhost:5173/tests/ui/game-page-tabs.html`, and select **Run navigation regression**.

This mounts the real game page with two installed game fixtures and no available
patch targets. It checks DLC and Launch & Runner navigation, all seven disabled
cards, background library refreshes that normalize local IDs, and both host
platform values. The final result must report 16 passing checks with no runtime
errors. It uses the browser preview API responses and never invokes native game
or patch operations. The fixture page is excluded from the production entry point.
