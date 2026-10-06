// Unit tests for the Claude cost table.
// Run: node --experimental-strip-types --test src/claudePricing.test.ts  (node >= 22.6)
// (Excluded from the app tsconfig -- this is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  costOf,
  priceFor,
  cacheHitRatio,
  totalPromptTokens,
  fmtUsd,
  fmtTokens,
  blendedRates,
  costAtBlended,
  PRICES,
  type TokenCounts,
} from "./claudePricing.ts";

const NOW = Math.floor(Date.UTC(2026, 8, 15) / 1000); // after every intro window
const EARLY = Math.floor(Date.UTC(2026, 7, 1) / 1000); // before 2026-08-31, Sonnet 5's old intro end

const zero: TokenCounts = {
  in_tokens: 0,
  out_tokens: 0,
  cache_read: 0,
  cache_write_5m: 0,
  cache_write_1h: 0,
};
const t = (o: Partial<TokenCounts>): TokenCounts => ({ ...zero, ...o });

test("a plain input/output bundle prices at the published per-million rate", () => {
  // 1M in + 1M out on Opus 5 = $5 + $25.
  const r = costOf(t({ in_tokens: 1_000_000, out_tokens: 1_000_000 }), "claude-opus-5", "standard", NOW);
  assert.equal(r.priced, true);
  assert.equal(r.usd, 30);
});

test("cache tokens use their multiples of the model's own input rate", () => {
  // Opus 5 input is $5/M. Read 0.1x -> $0.50, 5m write 1.25x -> $6.25,
  // 1h write 2x -> $10. Getting these three wrong is the single easiest way
  // to be off by a large factor on an agentic workload.
  const r = costOf(
    t({ cache_read: 1_000_000, cache_write_5m: 1_000_000, cache_write_1h: 1_000_000 }),
    "claude-opus-5",
    "standard",
    NOW,
  );
  assert.equal(r.usd, 0.5 + 6.25 + 10);
});

test("the 5m/1h write split actually changes the answer", () => {
  const asFiveMin = costOf(t({ cache_write_5m: 1_000_000 }), "claude-opus-5", "standard", NOW);
  const asOneHour = costOf(t({ cache_write_1h: 1_000_000 }), "claude-opus-5", "standard", NOW);
  assert.ok(asOneHour.usd > asFiveMin.usd, "a 1h write must cost more than a 5m one");
  assert.equal(asOneHour.usd / asFiveMin.usd, 2 / 1.25);
});

test("a dated snapshot id resolves to its family by longest prefix", () => {
  assert.equal(priceFor("claude-sonnet-4-6-20251114")?.in, 3);
  // And the longest prefix wins rather than the first match.
  assert.equal(priceFor("claude-opus-4-8")?.out, 25);
  assert.equal(priceFor("claude-opus-5")?.out, 25);
});

test("Sonnet 5 is a flat $2/$10 before and after 2026-08-31", () => {
  // The published page lists no expiry; a 3/15 post-intro rate would overstate by 50%.
  const spend = t({ in_tokens: 1_000_000, out_tokens: 1_000_000 });
  assert.equal(costOf(spend, "claude-sonnet-5", "standard", EARLY).usd, 12);
  assert.equal(costOf(spend, "claude-sonnet-5", "standard", NOW).usd, 12);
});

test("cache reads use the per-model multiple where the page publishes one", () => {
  // 1M cache-read tokens: the page's Hits column per model. A flat 0.1x would
  // overstate Fable/Mythos 5.1 by 4x and Opus 5.5 by 2x.
  const read = t({ cache_read: 1_000_000 });
  const usd = (m: string) => costOf(read, m, "standard", NOW).usd;
  assert.equal(usd("claude-opus-5"), 0.5);
  assert.equal(usd("claude-opus-5-5"), 0.2);
  assert.equal(usd("claude-fable-5-1"), 0.25);
  assert.equal(usd("claude-mythos-5-1"), 0.25);
  // Prefix guard: Fable 5 must not pick up the 5.1 multiple.
  assert.equal(usd("claude-fable-5"), 1);
});

test("a dated Fable 5.1 id resolves to the 5.1 row, not Fable 5", () => {
  assert.equal(priceFor("claude-fable-5-1-20261001"), PRICES["claude-fable-5-1"]);
});

test("fast mode is its own rate where one is published", () => {
  const spend = t({ in_tokens: 1_000_000, out_tokens: 1_000_000 });
  const std = costOf(spend, "claude-opus-5", "standard", NOW);
  const fast = costOf(spend, "claude-opus-5", "fast", NOW);
  assert.equal(std.usd, 30);
  assert.equal(fast.usd, 60);
  assert.equal(fast.fastRateAssumed, false);
});

test("fast mode on a model with no published fast rate is flagged, not invented", () => {
  const r = costOf(t({ in_tokens: 1_000_000 }), "claude-haiku-4-5", "fast", NOW);
  assert.equal(r.usd, 1, "must fall back to the standard rate");
  assert.equal(r.fastRateAssumed, true, "and say that it did");
});

test("an unknown model costs 0 and says it is unpriced", () => {
  const r = costOf(t({ in_tokens: 5_000_000 }), "claude-something-unreleased", "standard", NOW);
  assert.equal(r.usd, 0);
  assert.equal(r.priced, false);
  // Never guess. A wrong dollar figure is worse than an honest blank.
});

test("cache hit ratio counts every prompt token, not just the uncached ones", () => {
  const tok = t({ in_tokens: 100, cache_read: 900, cache_write_5m: 0 });
  assert.equal(totalPromptTokens(tok), 1000);
  assert.equal(cacheHitRatio(tok), 90);
  // An empty window must not divide by zero.
  assert.equal(cacheHitRatio(zero), 0);
});

test("output tokens are not prompt tokens", () => {
  // A regression guard: folding output into the ratio would make a chatty
  // session look like a cache miss.
  assert.equal(totalPromptTokens(t({ out_tokens: 5000, in_tokens: 10 })), 10);
});

test("fmtUsd keeps sub-cent amounts legible instead of rounding them to zero", () => {
  assert.equal(fmtUsd(0), "$0");
  assert.equal(fmtUsd(0.0004), "$0.0004");
  assert.equal(fmtUsd(0.42), "$0.420");
  assert.equal(fmtUsd(12.3456), "$12.35");
  assert.equal(fmtUsd(12345.6), "$12,346");
});

test("fmtTokens compacts the big numbers", () => {
  assert.equal(fmtTokens(812), "812");
  assert.equal(fmtTokens(3400), "3.4K");
  assert.equal(fmtTokens(1_250_000), "1.3M");
});

test("every table entry has both rates and any intro window is well-formed", () => {
  for (const [id, p] of Object.entries(PRICES)) {
    assert.ok(p.in > 0 && p.out > 0, `${id} is missing a rate`);
    assert.ok(p.out >= p.in, `${id}: output should not be cheaper than input`);
    if (p.intro) {
      assert.ok(p.intro.in <= p.in, `${id}: intro input rate is not a discount`);
      assert.ok(p.intro.out <= p.out, `${id}: intro output rate is not a discount`);
      assert.ok(Number.isFinite(p.intro.until), `${id}: intro window has no end`);
    }
    if (p.cacheReadMult !== undefined) {
      assert.ok(p.cacheReadMult > 0 && p.cacheReadMult <= 0.1, `${id}: cache-read multiple out of range`);
    }
    if (p.fast) {
      assert.ok(p.fast.in >= p.in, `${id}: fast mode should not be cheaper`);
    }
  }
});

test("blended rates are exact when the window used a single model", () => {
  const rows = [
    { key: "claude-opus-5", speed: "standard", in_tokens: 1_000_000, out_tokens: 1_000_000,
      cache_read: 0, cache_write_5m: 0, cache_write_1h: 0 },
  ];
  const r = blendedRates(rows, NOW);
  assert.equal(r.priced, true);
  assert.equal(r.in * 1_000_000, 5);
  assert.equal(r.out * 1_000_000, 25);
  assert.equal(costAtBlended(t({ in_tokens: 1_000_000, out_tokens: 1_000_000 }), r), 30);
});

test("blended rates weight by tokens, not by row count", () => {
  // 900k Haiku input ($1/M) and 100k Opus input ($5/M) must land near Haiku,
  // not halfway between the two rates.
  const rows = [
    { key: "claude-haiku-4-5", speed: "standard", in_tokens: 900_000, out_tokens: 0,
      cache_read: 0, cache_write_5m: 0, cache_write_1h: 0 },
    { key: "claude-opus-5", speed: "standard", in_tokens: 100_000, out_tokens: 0,
      cache_read: 0, cache_write_5m: 0, cache_write_1h: 0 },
  ];
  const r = blendedRates(rows, NOW);
  const perM = r.in * 1_000_000;
  assert.ok(Math.abs(perM - 1.4) < 1e-9, `want 1.4 got ${perM}`);
});

test("an all-unpriced window reports unpriced rather than zero-priced", () => {
  const rows = [
    { key: "claude-unreleased-9", in_tokens: 1000, out_tokens: 1000,
      cache_read: 0, cache_write_5m: 0, cache_write_1h: 0 },
  ];
  const r = blendedRates(rows, NOW);
  assert.equal(r.priced, false);
  assert.equal(costAtBlended(t({ in_tokens: 5000 }), r), 0);
});
