import type { HighlightRule } from "./stores/highlightStore";

interface CompiledRule {
  regex: RegExp;
  open: string;
  close: string;
}

function hexToRgb(hex: string): string | null {
  if (!/^#[0-9a-fA-F]{6}$/.test(hex)) return null;
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `${String(r)};${String(g)};${String(b)}`;
}

const MAX_PATTERN_LEN = 1024;
const NESTED_QUANTIFIER_RE = /(\+|\*|\{[\d,]+\})\)?(\+|\*|\{[\d,]+\})/;
const ALTERNATION_QUANTIFIER_RE = /\([^)]*\|[^)]*\)[+*]/;

function isSafePattern(pattern: string): boolean {
  if (pattern.length > MAX_PATTERN_LEN) return false;
  if (NESTED_QUANTIFIER_RE.test(pattern)) return false;
  if (ALTERNATION_QUANTIFIER_RE.test(pattern)) return false;
  return true;
}

function compileRule(rule: HighlightRule): CompiledRule | null {
  try {
    const rgb = hexToRgb(rule.color);
    if (rgb === null) return null;
    if (!isSafePattern(rule.pattern)) return null;
    const flags = `gm${rule.case_sensitive ? "" : "i"}`;
    // eslint-disable-next-line security/detect-non-literal-regexp -- validated by isSafePattern()
    const regex = new RegExp(rule.pattern, flags);
    const boldPrefix = rule.bold ? "1;" : "";
    const boldSuffix = rule.bold ? "22;" : "";
    return {
      regex,
      open: `\x1b[${boldPrefix}38;2;${rgb}m`,
      close: `\x1b[${boldSuffix}39m`,
    };
  } catch {
    return null;
  }
}

export class HighlightEngine {
  private rules: CompiledRule[];

  constructor(rules: HighlightRule[]) {
    this.rules = rules.map(compileRule).filter((r): r is CompiledRule => r !== null);
  }

  process(text: string): string {
    if (this.rules.length === 0) return text;
    let result = text;
    for (const rule of this.rules) {
      try {
        rule.regex.lastIndex = 0;
        result = result.replace(rule.regex, (m) => `${rule.open}${m}${rule.close}`);
      } catch {
        // Skip rules that fail at runtime (e.g. catastrophic backtracking)
      }
    }
    return result;
  }
}
