// Phase 12.A: small SolidJS-friendly i18n.
//
// Dictionaries are statically imported (no async loader — the app already
// ships every translation in the bundle, total ~30 KB) and the active
// language + direction live in two signals so both `t(key)` and the
// document `dir` attribute react together.

import { createSignal, createMemo } from "solid-js";

import en from "./en.json";
import he from "./he.json";
import ar from "./ar.json";
import ru from "./ru.json";

export type Language = "en" | "he" | "ar" | "ru";
export type Direction = "auto" | "ltr" | "rtl";
export type ResolvedDirection = "ltr" | "rtl";

const DICTS: Record<Language, Record<string, string>> = { en, he, ar, ru };
const RTL_LANGS: ReadonlySet<Language> = new Set(["he", "ar"] as Language[]);

export const LANGUAGES: { id: Language; label: string }[] = [
  { id: "en", label: "English" },
  { id: "he", label: "עברית" },
  { id: "ar", label: "العربية" },
  { id: "ru", label: "Русский" },
];

const [language, setLanguageInternal] = createSignal<Language>("en");
const [directionPref, setDirectionPref] = createSignal<Direction>("auto");

export const currentLanguage = language;
export const currentDirectionPref = directionPref;

export const resolvedDirection = createMemo<ResolvedDirection>(() => {
  const d = directionPref();
  if (d === "ltr" || d === "rtl") return d;
  return RTL_LANGS.has(language()) ? "rtl" : "ltr";
});

/**
 * Look up a key in the active dictionary. English is the fallback for
 * any missing key (so half-translated dictionaries still produce valid
 * output). `vars` are interpolated as `{name}` substitutions.
 */
export function t(key: string, vars?: Record<string, string | number>): string {
  const dict = DICTS[language()] ?? DICTS.en;
  const raw = dict[key] ?? DICTS.en[key] ?? key;
  if (!vars) return raw;
  return raw.replace(/\{(\w+)\}/g, (_, name) =>
    vars[name] !== undefined ? String(vars[name]) : `{${name}}`
  );
}

/** English text of a key, whatever the active language. Not reactive. */
export function tEn(key: string): string {
  return DICTS.en[key] ?? key;
}

/** Set both language + direction preference and apply to <html>. */
export function setLanguage(next: Language, dir?: Direction): void {
  setLanguageInternal(next);
  if (dir !== undefined) setDirectionPref(dir);
  applyToDocument();
}

/** Set just the direction preference and re-apply. */
export function setDirection(dir: Direction): void {
  setDirectionPref(dir);
  applyToDocument();
}

/**
 * Write `lang` and `dir` onto <html> so CSS logical properties +
 * native form controls + browser shortcuts behave correctly.
 */
export function applyToDocument(): void {
  const html = document.documentElement;
  html.setAttribute("lang", language());
  html.setAttribute("dir", resolvedDirection());
}

/** Apply directly from a Settings.i18n shape, used at startup + on changes. */
export function applyI18nSettings(s: { language?: string; direction?: string } | undefined): void {
  if (!s) return;
  const lang = (s.language as Language) || "en";
  const dir = (s.direction as Direction) || "auto";
  if (DICTS[lang]) setLanguageInternal(lang);
  if (dir === "auto" || dir === "ltr" || dir === "rtl") setDirectionPref(dir);
  applyToDocument();
}
