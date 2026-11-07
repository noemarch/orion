/**
 * UI text. The early build ships one language: the English string is the key and the text.
 * `t("Rename pane")` and `t("Worked for {duration}", { duration })` already read the way the
 * components need, so adding a language later is a dictionary and a menu entry, not a rewrite.
 *
 * Components take `t` from `useT()`, whose identity changes with the language so memoized work
 * is redone. Helpers outside React call the module-level `t`, which reads the language the
 * SettingsProvider last set.
 */
import { useMemo } from "react";

// in the order the Language menu lists them once translated builds exist
export const LANGUAGE_SETTINGS = ["system", "en"] as const;
export type LanguageSetting = (typeof LANGUAGE_SETTINGS)[number];
/** The languages a future build can carry. Only English ships now, so nothing resolves to them. */
export type Language = "en" | "ko" | "ja" | "zh";

export const LANGUAGE_NAMES: Record<LanguageSetting, string> = { system: "System", en: "English" };

/** The BCP 47 tag for `<html lang>` and Intl formatting. */
export const LOCALE_TAGS: Record<Language, string> = { en: "en-US", ko: "ko-KR", ja: "ja-JP", zh: "zh-CN" };

/** Resolves `system` to a language. Only English ships, so every setting resolves to English. */
export function resolveLanguage(_setting: LanguageSetting, _languages?: readonly string[]): Language {
  return "en";
}

let current: Language = "en";

export function setCurrentLanguage(language: Language): void {
  current = language;
}

/** The BCP 47 tag of the language the SettingsProvider last set, for Intl outside React. */
export function currentLocale(): string {
  return LOCALE_TAGS[current];
}

export type Vars = Record<string, string | number>;

/** The text for `key`, placeholders filled; pure, for tests and for both `t`s. */
export function translate(_language: Language, key: string, vars?: Vars): string {
  return vars === undefined ? key : key.replace(/\{(\w+)\}/g, (match, name: string) => (name in vars ? String(vars[name]) : match));
}

export function t(key: string, vars?: Vars): string {
  return translate(current, key, vars);
}

export type Translate = (key: string, vars?: Vars) => string;

/** `t` bound to the current language; a new function whenever the language changes. */
export function useT(): Translate {
  return useMemo<Translate>(() => (key, vars) => translate(current, key, vars), []);
}

/** The BCP 47 tag for Intl formatting in the current language. */
export function useLocale(): string {
  return LOCALE_TAGS[current];
}
