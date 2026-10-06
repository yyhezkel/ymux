# ymux-app-src-insightsanalytics-tsx — research report

## Question
FOLLOWUPS.md carries a P1 entry ("THE ANALYTICS TAB HAS NEVER BEEN RUN", FOLLOWUPS.md:51 at HEAD e2219a3) asking for a live walk of the Monitor → Analytics tab (`app/src/InsightsAnalytics.tsx`) and its daemon endpoint (`app/src-tauri/server/internal/insights/analytics.go`). The goal asks eight questions about it. **This research stage ran headless on a Linux box with no desktop app, no WebView2, no remote workspace and no clipboard, and Rule #17 bans local builds.** So no question could be answered by a live run. Every answer below is a static reading of the code paths the live walk would go through. Each one says what the code predicts, plus any defect the reading found. The live confirmation for each question is listed under Open questions. No live run is recorded anywhere: `grep -i analytics PROGRESS.txt` returns only the build and vault entries (PROGRESS.txt:5841, 6063, 6316–6366), and the FOLLOWUPS entry is still `[~]`.

One correction to the goal first. **The range picker has no 1d/30d/90d.** The ranges are `1h / 6h / 24h / 7d` (`app/src/InsightsAnalytics.tsx:35-41`), and the server clamps every window to the 7-day retention (`analytics.go:28,391-393`, `store.go:11 retentionDays = 7`). "30d" and "90d" in the goal came from intake. The original request only says "walk all four ranges". Q2 is answered against the real four.

### Q1 — Does the Analytics tab load without errors on a remote workspace?
### Q2 — Do all four date ranges (1d, 7d, 30d, 90d) display correct axis labels matching the selected range?
### Q3 — Does the by_disk `growth` column accurately calculate mount growth without false positives?
### Q4 — Do tooltips render correctly at non-default panel widths and in floating windows?
### Q5 — Does the RTL panel render correctly in Hebrew with numbers staying LTR?
### Q6 — Does a local workspace show the "needs daemon" panel correctly instead of an error?
### Q7 — Does a workspace with an older daemon show a 404 hint properly?
### Q8 — Does the "Copy for Claude" feature work and produce a reasonable paste size?

## Findings

### Q1
**Answer (static, not observed live):** the code path is coherent end to end, and nothing in it predicts an error on a current daemon. Whether it actually loads is still unobserved.

- The panel calls `insights_fetch` with `/analytics?since=<now-range>&points=120` (`InsightsAnalytics.tsx:105-110`). It loads once on mount and again on range or workspace change, through a pinned `on([...])` effect (`:128-133`). There is no polling.
- `insights_fetch` runs one `curl -s --max-time 6` over the workspace SSH session. It returns the body on HTTP 200 and maps every other status to a fixed error string (`app/src-tauri/src/addons.rs:892-920`).
- The daemon registers `/analytics` and `/api/v2/insights/analytics` behind auth (`server/internal/insights/service.go:55,64-67`). Out-of-range parameters are clamped, never rejected (`analytics.go:381-407`).
- Daemon side, the endpoint makes five SQL rollups per request (`analytics.go:176-190`). Go tests cover a whole-window aggregate over 4,320 samples, long-window end coverage, rollups, day-bucket switching, an empty store, range clamping and a missing store (`analytics_test.go:52,96,120,158,173,205,241`). Per CLAUDE.md those tests run only in CI.
- **Unmeasured risk:** the 6 s curl budget (`addons.rs:893`) against a 7-day window. That is ~120k `samples` rows (the panel comment says so, `InsightsAnalytics.tsx:27`), plus `disk_samples`, which has one row per mount per tick (`store.go:75-77`). Only `idx_disk_ts` exists on it (`store.go:45`). The disk query runs two correlated subqueries per group, filtered on `mount` + `ts` (`analytics.go:301-311`). With only a `ts` index, each subquery rescans the window's rows. A slow daemon surfaces as "insights daemon not reachable … (is it running?)", because curl times out with code `000` (`addons.rs:918`). That message would point at the wrong cause. The daemon logs `took_ms` at debug level (`analytics.go:420-422`), so a live run can measure the real latency.

**Confidence:** medium for "no code-path error", low for "loads in time at 7d". The latency is the first number a live run should read.

### Q2
**Answer (static):** the labels print the right window, but **the plot can cover a different one**. This is exactly the failure mode the request warns about, and the code reproduces it whenever the store holds less history than the range.

- Axis labels are `fmtTime(r.since)` and `fmtTime(r.until)` (`InsightsAnalytics.tsx:462-466`). Those are the server's clamped request bounds (`analytics.go:166-167`), not the first and last sample.
- The x position of each point comes from its **array index**, not its timestamp: `xAt(i) = PAD + i*(W-2PAD)/(n-1)` (`InsightsAnalytics.tsx:157`). Take a daemon that has been up 2 days and the 7d range: ~34 buckets get stretched across the full width, under a left label that reads "7 days ago". The same happens with any gap inside the window, such as daemon restarts or a stopped sampler: the gap is closed up and the line runs continuously through it. The only guard is the Samples tile. It turns amber when coverage is below 50% (`:389-397`, coverage formula `:192-198`). Coverage is `last_ts - first_ts` over the span, so it does not see gaps in the middle.
- Per-range label format. The date is added only when `rangeSeconds() > 86400` (`:171-179`), so 1h/6h show clock times only (correct), and 7d shows day/month + time (correct). **24h shows clock time only**, so both ends read the same "HH:MM". That is correct, but someone checking the labels against the range can't tell it apart from a broken 0-length window.
- Period table: hour buckets below 48h and day buckets at 48h and above (`analytics.go:159-162`), capped at 24 rows (`:278`). Under that rule 1h/6h/24h get hourly rows and 7d gets daily rows, and the header switches by `period_s` (`InsightsAnalytics.tsx:498-500`). **Day buckets are UTC days**, `(ts/86400)*86400` (`analytics.go:275`), while the comment says the keys are formatted "in the VIEWER's timezone" (`:155-158`). Only the label is viewer-local, not the boundary. In Israel (UTC+2/+3) the row labelled "06/10" covers 06/10 02:00/03:00 to 07/10 02:00/03:00 local time. A viewer west of UTC would see the previous date.
- Series buckets are epoch-aligned, `(ts/bucket)*bucket` (`analytics.go:248`). For 7d, bucket = 604800/120 = 5040 s, so the first point's timestamp can sit up to 5040 s before `since`. The result is up to 121 rows and a first tooltip time slightly earlier than the left axis label. This is harmless.

**Confidence:** high that the index-based x-axis misplaces partial or gappy windows (the code is unambiguous). Whether it shows on Yossi's servers depends on daemon uptime, which a live run reads straight off the Samples tile.

### Q3
**Answer (static):** the feared false positive does **not** follow from the SQL. A mount that appears in the middle of the window gets growth = its own last `used` minus its own first `used` within the window, not minus zero. The column has three other real weaknesses, though.

- The subqueries filter `f.mount = d.mount AND f.ts BETWEEN since AND until`, ordered ASC/DESC `LIMIT 1` (`analytics.go:303-308`). "First" is the mount's first appearance. A mount that first appears at t=6d starts its delta from its reading at 6d. No zero baseline exists anywhere: rows are written only for mounts that were present, with `u.Total > 0` (`sampler.go:168-181`, `store.go:75-77`).
- **Real weakness 1 — no span shown.** The row's `n` is in the payload (`analytics.go:94`) but not rendered (`InsightsAnalytics.tsx:546-563`). "+2 GB" over 10 minutes and "+2 GB" over 7 days look the same in the table.
- **Real weakness 2 — a remount or device swap at the same mountpoint.** The key is the mountpoint string only. If a different filesystem is mounted at the same path in the middle of the window (USB or NFS remount, a resized volume), the delta mixes two devices and can be huge. `total` is `MAX(d.total)` (`:302`), so even pct_last is computed against the larger device.
- **Real weakness 3 — bind and container mounts.** `disk.Partitions(false)` keeps every mount whose fstype is not in the virtual list (`sampler.go:168-173,297-303`). `overlay` is filtered, but bind mounts of a real filesystem (ext4/xfs) are not. Docker volume binds and snap mounts can come and go with containers. Each such row reports the growth of the **underlying** filesystem over its own lifetime, so the same bytes show up again under several mountpoints. Any positive growth, even 1 byte, is painted amber (`InsightsAnalytics.tsx:554`).
- Test coverage is one mount present for the whole window (`analytics_test.go:31-32,128-135`). No test seeds a late-appearing mount or a remount.

**Confidence:** high on the arithmetic (it reads straight off the SQL). Medium on how often weaknesses 2 and 3 happen on real servers: that depends on their mount tables, which nobody here has looked at.

### Q4
**Answer (static):** the clientX→viewBox conversion is width-independent and float-safe by construction. One edge case remains.

- `vx = (clientX - box.left)/box.width * W` uses `getBoundingClientRect()` on the `<svg>` itself (`InsightsAnalytics.tsx:204-213`). `preserveAspectRatio="none"` (`:420`) makes the viewBox→pixel mapping purely linear on x, so this holds at any rendered width. `clientX` and `getBoundingClientRect()` are both viewport coordinates, and the rect already includes ancestor CSS transforms, so a floating or dragged window does not shift it.
- The tooltip is placed at `left: xAt(i)/W * 100%` inside `.ins-an-chart { position: relative }` (`:446-454`, `app/src/App.css:6220,6233-6234`). It flips to `translateX(-100%)` past the midpoint, so it stays inside the chart box.
- **Edge case:** the tooltip sits at `top: 4px` (`App.css:6234`) on a 130 px-tall chart (`:6222`). Near the peak, the hovered dot can sit under the tooltip. Under `.ins-an.narrow` (panel below 560 px, `InsightsAnalytics.tsx:85`) a long fmtBps string plus a dated 7d time can be wider than half the chart. It then runs past the opposite edge, because the flip only decides the side. This is cosmetic.
- Fullscreen versus drawer versus float: each is just another width for this code. The one assumption that needs a live check is that a float's `.ins-an-chart` is not inside a `transform: scale()` that the hover handler can't see. It would be fine even then, because `getBoundingClientRect` includes transforms.

**Confidence:** high for the math. Medium for the visual overlap, which needs one live glance.

### Q5
**Answer (static):** numbers are pinned LTR. **The table bars will mirror under Hebrew**, which is what the request says must not happen, and number cells will be misaligned against their headers.

- LTR pins: `.ins-an-num { direction: ltr }` (`App.css:6212`), chart (`:6220`), tooltip (`:6235`) and axis row (`:6242`). Every numeric cell, including mount paths and container names, carries `ins-an-num` (`InsightsAnalytics.tsx:517-522,549-560,594-607`). The document gets `dir="rtl"` under Hebrew (`app/src/i18n/index.ts:75`), and all `insights.an.*` keys exist in `he.json` (e.g. `he.json:917-928`).
- **Bars mirror:** `.ins-an-bar` is a block `div` with a `%` width inside the `td` (`InsightsAnalytics.tsx:272-281`, `App.css:6264-6268`). Neither the bar cell nor the table is pinned to `direction: ltr`. A block narrower than its container in an RTL context sits at the inline-start (right) edge, so in Hebrew every bar grows right-to-left.
- **Header/value misalignment:** `th` is `text-align: start` (`App.css:6254`), which resolves to right under RTL. Each `td.ins-an-num` has its own `direction: ltr`, so its `start` resolves to left. Headers end up right-aligned over left-aligned numbers. Cell padding is physical, `4px 8px 4px 0` (right side) (`:6254,6259`), so in RTL the gutter is on the wrong side of each column.
- Column order mirrors (mount first column on the right). That is normal RTL table behaviour, and the request doesn't object to it.
- The tile sub-lines (e.g. `insights.an.tile.mem_sub` with `{peak}`/`{total}`) are not wrapped in `ins-an-num` (`InsightsAnalytics.tsx:267`), so mixed Hebrew+number strings go through the bidi algorithm. That is usually fine, but it is unpinned.

**Confidence:** high that the bar direction follows the document direction (standard CSS block placement, nothing here overrides it). Medium on how bad the misalignment looks. One screenshot under Hebrew settles both.

### Q6
**Answer (static):** yes. A local workspace gets the "needs the daemon" panel and no error.

- `insights_fetch` routes local workspaces in-process before any SSH (`addons.rs:877-884`). `/analytics` maps to `insights_local_analytics()`, which returns `Ok({"unavailable":"local"})` (`app/src-tauri/src/insights_local.rs:648-656,671`). That is an `Ok`, so `err` stays null.
- The panel sets `localOnly = rep()?.unavailable === "local"` (`InsightsAnalytics.tsx:138`). It renders `insights.an.local_title` and `local_hint` (`:336-341`). The "no data" fallback is suppressed with `!err() && !localOnly()` (`:346`). Copy for Claude is disabled because `hasData()` is false when `localOnly` (`:142,302`), and `copyForClaude` also returns early (`:234`).
- The "Copy commands" button stays enabled and passes `local: true` (`:250-257,309-316`). That is by design, per the vault (`docs/vault/frontend-panes.md` § Monitoring).

**Confidence:** high (a pure string-marker path with no I/O).

### Q7
**Answer (static):** yes for a daemon older than Phase 84.C. The old daemon has no root catch-all, so an unknown path is a plain ServeMux 404, which becomes `"insights daemon returned HTTP 404"` (`addons.rs:919`). That matches `/HTTP 40[34]/` (`InsightsAnalytics.tsx:140`), and the panel shows the raw line **plus** the `insights.an.old_daemon` hint (`:327-334`, `he.json:927`).

- Old-daemon routing: `git grep 'HandleFunc("/"\|Handle("/"' a311a95^ -- app/src-tauri/server` → no matches, meaning the tree just before Phase 84.C (`a311a95`, 2026-08-23) had no root handler. At HEAD the only routes are explicit (`service.go:52-67`, `term/service.go:123-135`), and the Phase 108 web bundle (`5ee38e3`, 2026-10-05) is later than `/analytics` anyway.
- The panel still prints the raw string ("✗ insights daemon returned HTTP 404") above the hint. The request asked for "the 404 hint rather than a raw error string". The code shows **both**, not the hint alone.
- **Dead arm:** the regex also matches `403`, but `addons.rs:917` maps 401/403 to "insights daemon rejected the token — reinstall the add-on", which contains no "HTTP 403". The `3` in the regex can never fire. That is harmless, because the token message is the right one for a 403.

**Confidence:** high on the mapping. Medium on "an old daemon really answers 404 and not 401". That depends on mux order: an unregistered path never reaches the auth wrapper, which wraps only registered handlers (`service.go:64-66`). It reads as 404, but nobody has observed it.

### Q8
**Answer (static):** the clipboard path has a fallback and reports failure in the UI. A 7d paste is predicted at roughly **10–13 KB (~3–4k tokens)**, a sane size. WebView2 granting the write is unobserved.

- `copyText` tries `navigator.clipboard.writeText` first. On a throw it falls back to an off-screen `<textarea>` + `execCommand("copy")` and returns a boolean (`app/src/clipboardText.ts:12-33`). Its header comment says WebView2 grants write and denies read (`:3-7`). That is a claim in a code comment, not a measured fact. A `false` shows `insights.an.copy_failed` (`InsightsAnalytics.tsx:242,323-325`). The call runs inside a click handler (`:304`), so the user-activation requirement is met.
- Size, worked out from the builder rather than measured. Series rows come from `textTable` with 8 columns joined by two spaces (`insightsReport.ts:112-122,200-216`). The widths are a 16-char `stamp` (`:103-109`), `cpu%` 5, `cpu_peak%` 9, `ram%` 5, `load` 4–5, `rx/tx_KB/s` 7–8 each and `n` ≤ 4 (5040 s / 5 s = 1008), so a row is ≈ 75 bytes. For 7d that is ≤ 121 rows (Q2), ≈ 9 KB. Add the intro, totals (6 rows), ≤ 24 period rows (8 for 7d), disks, and ≤ 20 containers (`analytics.go:341`), and the total is roughly 10–13 KB. The bound comes from the column widths. It was not computed by running the function, because Rule #17 bans local test runs.
- Content is WYSIWYG for the selected range (`InsightsAnalytics.tsx:228-246`). Under Hebrew the `intro` and `rangeLabel` are Hebrew inside an otherwise English report (`:237-239`, `insightsReport.ts:146-149`), which is readable but mixed-direction.

**Confidence:** medium on size (an estimate with a stated derivation). Low on the WebView2 grant until it is pressed once.

## Recommendation
Close the FOLLOWUPS entry only after the live walk, but **fix two code defects first**, because the walk will otherwise "pass" on a long-running server and miss them:

1. **Q2 — plot by timestamp, not index.** `xAt` at `app/src/InsightsAnalytics.tsx:157` (and the matching `onMove` index math at `:209-212`) should map `pt.t` into `[since, until]`, so a partial or gappy window shows as empty chart space. This is the request's own "#1 most likely wrong" item, and the code gets it wrong exactly when history is shorter than the range.
2. **Q5 — pin the bar cells LTR.** `.ins-an-barcell` / `.ins-an-bar` (`app/src/App.css:6264-6268`) should get `direction: ltr`, and number cells need an alignment that agrees with their headers.

Lower priority: render the disk row's `n`/span, or suppress growth when a mount's first sample is well after `since` (`InsightsAnalytics.tsx:552-558`). Also mention UTC day boundaries in the by-day header, or bucket by viewer offset (`analytics.go:275`). For the live walk, run it on a server whose daemon has been up **under** 7 days, so Q2's failure is actually exercised, and read `took_ms` from the daemon debug log for the 7d query (Q1 latency).

Trade-off accepted: these findings are static. What would change this recommendation: a live 7d run on a full-history server that renders in under 6 s with correct labels makes fix 1 a robustness fix rather than a bug fix. A Hebrew screenshot whose bars still run left-to-right would mean some ancestor already pins direction and fix 2 can be dropped.

## Open questions
Every question needs a live confirmation that this headless stage could not do. These need Yossi or a desktop session on Windows with a remote workspace:

- **Q1:** open Analytics on a remote workspace and note any error. Read the daemon's `analytics ok … took_ms` debug line for 7d (`analytics.go:420-422`) to check it fits inside curl's 6 s (`addons.rs:893`).
- **Q2:** on a daemon up for less than 7 days, pick 7d and check whether the line spans the full width under a "7 days ago" label (predicted yes). Check the 24h end labels. Compare a by-day row's label with its local-time coverage.
- **Q3:** on a server with Docker bind mounts or a remounted path, compare the by_disk rows with `findmnt` and with growth measured by hand.
- **Q4:** hover at drawer width, below 560 px, in a float and in fullscreen. Check that cursor and dot line up and that the tooltip doesn't clip.
- **Q5:** switch to Hebrew and screenshot the period, disk and container tables (predicted: bars grow right-to-left, headers misaligned).
- **Q6:** open Analytics on a local workspace (predicted: the needs-daemon panel, Copy for Claude disabled).
- **Q7:** point at a pre-`a311a95` daemon and confirm curl sees 404, not 401. Note that the raw line shows above the hint.
- **Q8:** press Copy for Claude in WebView2 on a 7d report, paste it, and measure its byte length against the predicted 10–13 KB.

## Sources
- app/src/InsightsAnalytics.tsx:35-41 — ranges are 1h/6h/24h/7d
- app/src/InsightsAnalytics.tsx:105-110,128-133 — fetch path and load trigger
- app/src/InsightsAnalytics.tsx:138-142 — localOnly / oldDaemon / hasData
- app/src/InsightsAnalytics.tsx:157-158 — index-based x mapping
- app/src/InsightsAnalytics.tsx:171-189 — axis and period time formats
- app/src/InsightsAnalytics.tsx:192-198,389-397 — coverage tile
- app/src/InsightsAnalytics.tsx:204-213 — clientX→viewBox
- app/src/InsightsAnalytics.tsx:232-246,302-325 — Copy for Claude flow
- app/src/InsightsAnalytics.tsx:272-281,546-563 — bar cell; disk row omits n
- app/src/InsightsAnalytics.tsx:327-341 — error, old-daemon and local panels
- app/src/App.css:6212,6220,6233-6242 — LTR pins on nums, chart, tip, axis
- app/src/App.css:6252-6268 — table/bar CSS without direction pin; physical padding
- app/src/clipboardText.ts:3-33 — writeText + execCommand fallback
- app/src/insightsReport.ts:103-122,142-216 — stamp, textTable, report layout
- app/src/i18n/index.ts:75 — html dir set from language
- app/src/i18n/he.json:917-928 — Hebrew range, old-daemon and local strings
- app/src-tauri/src/addons.rs:877-920 — local routing, curl --max-time 6, status→message map
- app/src-tauri/src/insights_local.rs:648-656,671 — `{"unavailable":"local"}`
- app/src-tauri/server/internal/insights/analytics.go:28-32,146-192 — clamp, bucket/period choice
- app/src-tauri/server/internal/insights/analytics.go:246-311 — series, period (UTC day) and disk SQL
- app/src-tauri/server/internal/insights/analytics.go:381-423 — handler clamps, debug took_ms
- app/src-tauri/server/internal/insights/store.go:11,42-45,75-77 — retention 7, disk_samples schema/index, insert
- app/src-tauri/server/internal/insights/sampler.go:167-181,297-303 — mount selection and virtual-FS filter
- app/src-tauri/server/internal/insights/service.go:52-67 — route table, auth wrap
- app/src-tauri/server/internal/insights/analytics_test.go:14-40,120-135 — single full-window mount seed; growth test
- `git grep 'HandleFunc("/"\|Handle("/"' a311a95^ -- app/src-tauri/server` → no matches (old daemon has no catch-all)
- `git log -S'/analytics' -- app/src-tauri/server` → a311a95 2026-08-23 Phase 84.C (endpoint introduced)
- `git log -S'www/current' -- app/src-tauri/server` → 5ee38e3 2026-10-05 Phase 108 (web bundle later than /analytics)
- `grep -n -i analytics PROGRESS.txt` → only build/vault entries (5841, 6063, 6316-6366), no live run
- `grep -n "ANALYTICS TAB HAS NEVER BEEN RUN" FOLLOWUPS.md` → line 51, still `[~]`
