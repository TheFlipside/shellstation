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

// Outer-quantifier alternative used in both ReDoS checks below. Note that
// `{n}` (exact count) is intentionally NOT included — a fixed repetition
// count doesn't create the inter-iteration backtracking ambiguity that
// makes `+`, `*`, `{n,}`, and `{n,m}` dangerous. The trailing comma in
// `\{\d*,\d*\}` requires a comma somewhere, excluding `{n}`.
const OUTER_QUANT = String.raw`(?:[+*]|\{\d*,\d*\})`;

// Catastrophic-backtracking shape #1 — group whose content is a single
// quantified atom (a single char, an escape like `\d`/`\w`, or a single
// character class), itself followed by an outer quantifier.
// Examples caught:   (a+)+   (.+)*   (\d+)+   ([abc]+)+   (\w*)+
// Examples allowed:  (?:/\d+)*    (?:\w+,)+    (abc)+
// A literal character inside the group anchors each outer iteration and
// breaks the chain that lets inner and outer quantifiers compete.
// eslint-disable-next-line security/detect-non-literal-regexp -- args are compile-time string literals concatenated at module load
const NESTED_QUANTIFIER_RE = new RegExp(
  String.raw`\((?:\?:)?(?:\\.|\[[^\]]*\]|[^\s()|[\\])` + OUTER_QUANT + String.raw`\)` + OUTER_QUANT,
);

// Catastrophic-backtracking shape #2 — group whose content is two or more
// character-class atoms (no literal anchors) followed by an outer
// quantifier. Examples caught: `(\d+\w+)+`, `(\w+\s+)*`, `(\d*\w*)+`.
// A literal character anywhere in the content breaks the chain (e.g.
// `(?:/\d+)*` has `/`, `(a+b+)+` has `a` and `b`) — those groups stay
// allowed. Trade-off: also flags `(?:\d\w)+`-style patterns with no inner
// quantifier; rare enough in practice to accept as a false positive.
// eslint-disable-next-line security/detect-non-literal-regexp -- args are compile-time string literals concatenated at module load
const CLASS_ONLY_QUANTIFIER_RE = new RegExp(
  String.raw`\((?:\?:)?(?:(?:\\[wdsWDS]|\.|\[[^\]]*\])` +
    OUTER_QUANT +
    String.raw`?){2,}\)` +
    OUTER_QUANT,
);

const ALTERNATION_QUANTIFIER_RE = /\([^)]*\|[^)]*\)[+*]/;

function isSafePattern(pattern: string): boolean {
  if (pattern.length > MAX_PATTERN_LEN) return false;
  if (NESTED_QUANTIFIER_RE.test(pattern)) return false;
  if (CLASS_ONLY_QUANTIFIER_RE.test(pattern)) return false;
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
        // Catches synchronously-thrown errors only. Catastrophic
        // backtracking in V8 does NOT throw — it hangs the thread. The
        // real defense against ReDoS is `isSafePattern` at compile time;
        // this catch only handles rare runtime exceptions (e.g. an engine
        // stack-overflow on extremely deep input).
      }
    }
    return result;
  }
}
