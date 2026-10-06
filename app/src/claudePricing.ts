// The one place ymux knows what Claude costs.
//
// The backends (`server/internal/insights/claudeusage.go` and
// `src/claude_usage_local.rs`) count tokens and deliberately do not price them:
// token counts are facts that never go stale, prices are a table that does.
// Keeping the table here means a price change is a one-file edit instead of a
// server rebake plus a matching edit in the Rust mirror.
//
// ⚠️ WHAT THE NUMBER MEANS. Claude Code on a Pro/Max subscription is NOT billed
// per token — the subscription is. Everything computed here is the
// API-EQUIVALENT cost: what these exact tokens would have cost at first-party
// API list price. It is the right number for "where is my quota going, in
// money terms" and the wrong number for "what will my card be charged". The UI
// must never label it as a bill.
//
// Rates are Anthropic first-party API list prices, USD per million tokens.
// Bedrock and Vertex are partner-operated with separate pricing and are not
// modelled here.

/** When the table was last checked against published pricing. */
export const PRICING_AS_OF = "2026-10-06";

export interface ModelPrice {
  /** USD per million input tokens. */
  in: number;
  /** USD per million output tokens. */
  out: number;
  /**
   * Optional promotional rate, applied while `until` (a unix second) has not
   * passed. Sonnet 5's launch pricing is the reason this exists — silently
   * charging the standard rate for a call made under the intro rate overstates
   * the bill by 50%.
   */
  intro?: { in: number; out: number; until: number };
  /**
   * Fast mode is a separate rate, not a multiplier. Only present for models
   * where a fast rate is published; anything else falls back to the standard
   * rate and is flagged, rather than being invented.
   */
  fast?: { in: number; out: number };
}

// 2026-08-31T23:59:59Z — the last moment Sonnet 5's intro pricing applies.
const SONNET5_INTRO_UNTIL = Date.UTC(2026, 7, 31, 23, 59, 59) / 1000;

/**
 * Keyed by model id prefix. Lookup takes the LONGEST matching prefix, so a
 * dated snapshot id (`claude-sonnet-4-5-20250929`) resolves to its family
 * without needing its own row.
 */
export const PRICES: Record<string, ModelPrice> = {
  "claude-fable-5": { in: 10, out: 50 },
  "claude-mythos-5": { in: 10, out: 50 },
  "claude-opus-5-5": { in: 4, out: 20, fast: { in: 8, out: 40 } },
  "claude-opus-5": { in: 5, out: 25, fast: { in: 10, out: 50 } },
  "claude-opus-4-8": { in: 5, out: 25, fast: { in: 10, out: 50 } },
  "claude-opus-4-7": { in: 5, out: 25 },
  "claude-opus-4-6": { in: 5, out: 25 },
  "claude-sonnet-5-5": { in: 2, out: 10 },
  "claude-sonnet-5": {
    in: 3,
    out: 15,
    intro: { in: 2, out: 10, until: SONNET5_INTRO_UNTIL },
  },
  "claude-sonnet-4-6": { in: 3, out: 15 },
  "claude-haiku-4-5": { in: 1, out: 5 },
};

// Cache pricing is a multiple of the model's own input rate, identical across
// models: a read is a tenth of an input token, a 5-minute write is 1.25x, and a
// 1-hour write is 2x. The 5m/1h split is why the backends report them
// separately — folding them together would understate any long session.
export const CACHE_READ_MULT = 0.1;
export const CACHE_WRITE_5M_MULT = 1.25;
export const CACHE_WRITE_1H_MULT = 2.0;

export interface TokenCounts {
  in_tokens: number;
  out_tokens: number;
  cache_read: number;
  cache_write_5m: number;
  cache_write_1h: number;
}

export interface CostResult {
  usd: number;
  /** False when the model is not in the table — `usd` is then 0, not a guess. */
  priced: boolean;
  /** True when a published fast rate was missing and the standard one was used. */
  fastRateAssumed: boolean;
}

/** Longest-prefix lookup, so dated snapshot ids resolve to their family. */
export function priceFor(model: string): ModelPrice | null {
  let best: ModelPrice | null = null;
  let bestLen = -1;
  for (const [prefix, price] of Object.entries(PRICES)) {
    if (model.startsWith(prefix) && prefix.length > bestLen) {
      best = price;
      bestLen = prefix.length;
    }
  }
  return best;
}

/**
 * API-equivalent cost of one bundle of tokens.
 *
 * `atUnix` is when the tokens were spent — promotional rates are time-bounded,
 * so pricing a week-old call at today's rate would be wrong.
 */
export function costOf(
  tokens: TokenCounts,
  model: string,
  speed: string | undefined,
  atUnix: number,
): CostResult {
  const price = priceFor(model);
  if (!price) return { usd: 0, priced: false, fastRateAssumed: false };

  let rate = { in: price.in, out: price.out };
  let fastRateAssumed = false;
  if (price.intro && atUnix <= price.intro.until) {
    rate = { in: price.intro.in, out: price.intro.out };
  }
  if (speed === "fast") {
    if (price.fast) {
      rate = { in: price.fast.in, out: price.fast.out };
    } else {
      // No published fast rate for this model: bill it at the standard rate
      // and say so, rather than inventing a premium.
      fastRateAssumed = true;
    }
  }

  const perM = (n: number, r: number) => (n / 1_000_000) * r;
  const usd =
    perM(tokens.in_tokens, rate.in) +
    perM(tokens.out_tokens, rate.out) +
    perM(tokens.cache_read, rate.in * CACHE_READ_MULT) +
    perM(tokens.cache_write_5m, rate.in * CACHE_WRITE_5M_MULT) +
    perM(tokens.cache_write_1h, rate.in * CACHE_WRITE_1H_MULT);

  return { usd, priced: true, fastRateAssumed };
}

/** Total prompt size = uncached + written + read. */
export function totalPromptTokens(t: TokenCounts): number {
  return t.in_tokens + t.cache_write_5m + t.cache_write_1h + t.cache_read;
}

/**
 * Share of prompt tokens served from cache. The interesting number for "am I
 * wasting money re-sending the same context": a low ratio on a long session
 * means the cache is being invalidated.
 */
export function cacheHitRatio(t: TokenCounts): number {
  const total = totalPromptTokens(t);
  return total > 0 ? (t.cache_read / total) * 100 : 0;
}

/** USD with enough precision to stay useful under a dollar. */
export function fmtUsd(n: number): string {
  if (n === 0) return "$0";
  if (Math.abs(n) < 0.01) return `$${n.toFixed(4)}`;
  if (Math.abs(n) < 1) return `$${n.toFixed(3)}`;
  if (Math.abs(n) < 1000) return `$${n.toFixed(2)}`;
  return `$${Math.round(n).toLocaleString("en-US")}`;
}

/** 1.2M / 3.4K / 812 — token counts are big and the exact digit rarely matters. */
export function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(Math.round(n));
}

/** A model row as the backends report it — used to derive blended rates. */
export interface ModelUsageRow extends TokenCounts {
  key: string;
  speed?: string;
  ended?: number;
}

export interface BlendedRates {
  /** USD per token, per category, for the model mix actually observed. */
  in: number;
  out: number;
  cacheRead: number;
  cacheWrite5m: number;
  cacheWrite1h: number;
  /** False when no row in the window had a known price. */
  priced: boolean;
}

/**
 * Effective per-token rates for the model mix in this window.
 *
 * Only the `by_model` rollup carries a model id — a project, a session or an
 * hour bucket can mix several. Rather than pick one and pretend, this derives
 * a token-weighted blend from the rows that DO know their model, category by
 * category. With a single model in the window it is exact; with several it is
 * an approximation, and the UI marks the columns that use it.
 */
export function blendedRates(rows: ModelUsageRow[], at: number): BlendedRates {
  const cat = (pick: (t: TokenCounts) => number, only: keyof TokenCounts) => {
    let tokens = 0;
    let usd = 0;
    for (const r of rows) {
      const n = pick(r);
      if (n <= 0) continue;
      const isolated: TokenCounts = {
        in_tokens: 0,
        out_tokens: 0,
        cache_read: 0,
        cache_write_5m: 0,
        cache_write_1h: 0,
        [only]: n,
      } as TokenCounts;
      const c = costOf(isolated, r.key, r.speed, r.ended || at);
      if (!c.priced) continue;
      tokens += n;
      usd += c.usd;
    }
    return tokens > 0 ? usd / tokens : 0;
  };

  const rates = {
    in: cat((t) => t.in_tokens, "in_tokens"),
    out: cat((t) => t.out_tokens, "out_tokens"),
    cacheRead: cat((t) => t.cache_read, "cache_read"),
    cacheWrite5m: cat((t) => t.cache_write_5m, "cache_write_5m"),
    cacheWrite1h: cat((t) => t.cache_write_1h, "cache_write_1h"),
  };
  const priced = Object.values(rates).some((r) => r > 0);
  return { ...rates, priced };
}

/** Apply blended rates to a bundle of tokens. */
export function costAtBlended(t: TokenCounts, r: BlendedRates): number {
  return (
    t.in_tokens * r.in +
    t.out_tokens * r.out +
    t.cache_read * r.cacheRead +
    t.cache_write_5m * r.cacheWrite5m +
    t.cache_write_1h * r.cacheWrite1h
  );
}
