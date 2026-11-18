/**
 * How text another user wrote is shown next to a decision: the characters
 * that would change how it displays are written out, not obeyed and not
 * dropped, a host is shown in printable ASCII, and neither is longer than
 * it is allowed to be.
 */
import { describe, expect, it } from "vitest";
import {
  PATH_SEPARATOR_CHAR,
  hostAsWritten,
  pathNameAsWritten,
  shownAsWritten,
  typedAsWritten,
} from "./displayText";

describe("shownAsWritten", () => {
  it("leaves ordinary text as it is", () => {
    for (const text of ["", "core-switch", "db.corp.example.com", "Büro ✓", "サーバー"]) {
      expect(shownAsWritten(text, 64)).toBe(text);
    }
    // Directional marks belong to names; see `DISPLAY_CONTROL_RE`.
    expect(shownAsWritten("edge\u200e\u200f", 64)).toBe("edge\u200e\u200f");
  });

  it("writes out a character that would reorder or hide text", () => {
    expect(shownAsWritten("\u202emoc.elpmaxe.live", 64)).toBe("\\u{202E}moc.elpmaxe.live");
    expect(shownAsWritten("good\u200b.example", 64)).toBe("good\\u{200B}.example");
    expect(shownAsWritten("a\u2066b\u2069c", 64)).toBe("a\\u{2066}b\\u{2069}c");
    expect(shownAsWritten("line\u2028break", 64)).toBe("line\\u{2028}break");
    expect(shownAsWritten("\ufeff\u00ad\u3164", 64)).toBe("\\u{FEFF}\\u{00AD}\\u{3164}");
  });

  it("writes out control characters", () => {
    expect(shownAsWritten("a\nb\tc\u0007\u009b", 64)).toBe(
      "a\\u{000A}b\\u{0009}c\\u{0007}\\u{009B}",
    );
  });

  it("leaves no such character in what it returns", () => {
    const all = String.fromCodePoint(
      0x00,
      0x1f,
      0x7f,
      0x9f,
      0xad,
      0x200b,
      0x2028,
      0x2029,
      0x202a,
      0x202e,
      0x2060,
      0x2064,
      0x2066,
      0x2069,
      0x3164,
      0xfeff,
      0xffa0,
      0xfff9,
      0xfffb,
    );
    // Nothing but written-out code points, one for each character.
    const written = shownAsWritten(all, 64).match(/\\u\{[0-9A-F]{4}\}/g) ?? [];
    expect(written).toHaveLength(Array.from(all).length);
    expect(written.join("")).toBe(shownAsWritten(all, 64));
  });

  it("cuts a long text, by character and before writing anything out", () => {
    expect(shownAsWritten("a".repeat(64), 64)).toBe("a".repeat(64));
    expect(shownAsWritten("a".repeat(65), 64)).toBe(`${"a".repeat(64)}…`);
    // A character outside the BMP counts once and is not split.
    expect(shownAsWritten("😀".repeat(5), 3)).toBe("😀😀😀…");
    // The cut is of the text as written, so no written-out code point is
    // cut in two.
    expect(shownAsWritten("ab\u202ecd", 3)).toBe("ab\\u{202E}…");
  });
});

describe("hostAsWritten", () => {
  it("leaves a host in printable ASCII as it is", () => {
    for (const host of ["", "db.corp.example.com", "10.0.0.2", "[fe80::1%eth0]", "a_b-c.d"]) {
      expect(hostAsWritten(host)).toBe(host);
    }
  });

  it("writes out a letter of another script that looks like a Latin one", () => {
    // A Cyrillic a in "bastion".
    expect(hostAsWritten("b\u0430stion.corp")).toBe("b\\u{0430}stion.corp");
    expect(hostAsWritten("bücher.example")).toBe("b\\u{00FC}cher.example");
  });

  it("writes out what a name may keep: marks, selectors, right-to-left letters", () => {
    expect(hostAsWritten("a\u200eb\u200fc")).toBe("a\\u{200E}b\\u{200F}c");
    expect(hostAsWritten("a\ufe0fb")).toBe("a\\u{FE0F}b");
    expect(hostAsWritten("\u05d0.example")).toBe("\\u{05D0}.example");
    // A tag character, outside the BMP, as one code point.
    expect(hostAsWritten(`a${String.fromCodePoint(0xe0041)}b`)).toBe("a\\u{E0041}b");
  });

  it("writes out control characters and the characters that hide or reorder", () => {
    expect(hostAsWritten("\u202emoc.elpmaxe.live")).toBe("\\u{202E}moc.elpmaxe.live");
    expect(hostAsWritten("good\u200b.example\n")).toBe("good\\u{200B}.example\\u{000A}");
  });

  it("returns nothing but printable ASCII for a host that is not cut", () => {
    const odd = `${String.fromCodePoint(0x00, 0x7f, 0x80, 0x202e, 0x430, 0x1f600)}host`;
    expect(hostAsWritten(odd)).toMatch(/^[ -~]*$/);
  });

  it("cuts a host that is longer than the app lets one be", () => {
    expect(hostAsWritten("a".repeat(255))).toBe("a".repeat(255));
    expect(hostAsWritten("a".repeat(256))).toBe(`${"a".repeat(255)}…`);
    expect(hostAsWritten("ssh-and-more", 3)).toBe("ssh…");
  });
});

describe("typedAsWritten", () => {
  it("leaves a user name in printable ASCII as it is", () => {
    for (const name of ["", "admin", "svc_backup-01", "first.last@example.com", "a b"]) {
      expect(typedAsWritten(name)).toBe(name);
    }
  });

  it("doubles a backslash, so that one that was written is told from one that was not", () => {
    expect(typedAsWritten("CORP\\admin")).toBe("CORP\\\\admin");
    // The text of a written-out character, and the character itself.
    expect(typedAsWritten("\\u{0430}")).toBe("\\\\u{0430}");
    expect(typedAsWritten(String.fromCodePoint(0x430))).toBe("\\u{0430}");
    expect(typedAsWritten("[\\]")).toBe("[\\\\]");
  });

  it("writes out everything else that is not printable ASCII", () => {
    expect(typedAsWritten("ro\rot\n")).toBe("ro\\u{000D}ot\\u{000A}");
    expect(typedAsWritten(`a${String.fromCodePoint(0x202e)}b`)).toBe("a\\u{202E}b");
    expect(typedAsWritten("ü")).toBe("\\u{00FC}");
  });

  it("cuts a user name that is longer than the app lets one be", () => {
    expect(typedAsWritten("u".repeat(255))).toBe("u".repeat(255));
    expect(typedAsWritten("u".repeat(256))).toBe(`${"u".repeat(255)}…`);
  });
});

describe("pathNameAsWritten", () => {
  const separator = String.fromCodePoint(0x203a);
  const ellipsis = String.fromCodePoint(0x2026);

  it("leaves a folder's name as it is, in any script", () => {
    expect(PATH_SEPARATOR_CHAR).toBe(separator);
    for (const name of ["", "Production", "10.0.0.0/24", "a > b", "Zürich", "東京"]) {
      expect(pathNameAsWritten(name, 64)).toBe(name);
    }
  });

  it("writes out what a path gives a meaning of its own", () => {
    // The separator: one folder is not to read as two, one in the other.
    expect(pathNameAsWritten(`Customers ${separator} Production`, 64)).toBe(
      "Customers \\u{203A} Production",
    );
    // The ellipsis, which stands for folders that are left out.
    expect(pathNameAsWritten(ellipsis, 64)).toBe("\\u{2026}");
    expect(pathNameAsWritten(`Lab${ellipsis}`, 64)).toBe("Lab\\u{2026}");
  });

  it("doubles a backslash, so that text is told from a character written out", () => {
    expect(pathNameAsWritten("CORP\\net", 64)).toBe("CORP\\\\net");
    expect(pathNameAsWritten("a\\u{203A}b", 64)).toBe("a\\\\u{203A}b");
    expect(pathNameAsWritten(`a${separator}b`, 64)).toBe("a\\u{203A}b");
  });

  it("writes out the characters that change how a name displays", () => {
    const override = String.fromCodePoint(0x202e);
    const zeroWidth = String.fromCodePoint(0x200b);
    expect(pathNameAsWritten(`a${override}b${zeroWidth}c\nd`, 64)).toBe(
      "a\\u{202E}b\\u{200B}c\\u{000A}d",
    );
  });

  it("cuts a name after as many characters as it is given, before writing any out", () => {
    expect(pathNameAsWritten("n".repeat(24), 24)).toBe("n".repeat(24));
    expect(pathNameAsWritten("n".repeat(25), 24)).toBe(`${"n".repeat(24)}…`);
    // The mark of the cut is the one ellipsis that is not written out.
    expect(pathNameAsWritten(`ab${separator}cd`, 3)).toBe("ab\\u{203A}…");
  });

  it("cuts by characters, also where they take two units each", () => {
    const face = String.fromCodePoint(0x1f600);
    expect(pathNameAsWritten(face.repeat(3), 3)).toBe(face.repeat(3));
    expect(pathNameAsWritten(face.repeat(4), 3)).toBe(`${face.repeat(3)}…`);
    expect(pathNameAsWritten(face.repeat(100), 3)).toBe(`${face.repeat(3)}…`);
    // A cut that would fall inside one takes neither half of it.
    expect(pathNameAsWritten(`a${face.repeat(100)}`, 2)).toBe(`a${face}…`);
    expect(pathNameAsWritten(`ab${face.repeat(100)}`, 2)).toBe("ab…");
  });

  it("cuts first and writes out what is left, a backslash and a control among it", () => {
    const override = String.fromCodePoint(0x202e);
    // Four characters of six: the two that are written out or doubled count
    // as one each, and what is past the cut is not shown in any form.
    expect(pathNameAsWritten(`a\\${override}b${separator}c`, 4)).toBe("a\\\\\\u{202E}b…");
    // A name's own ellipsis is written out; the one of the cut is not.
    expect(pathNameAsWritten(`${ellipsis}${ellipsis}x`, 2)).toBe("\\u{2026}\\u{2026}…");
  });

  it("goes through the start of a name only, however long the name is", () => {
    // Megabytes, as a row written past the app's own checks can have.
    const long = "n".repeat(8 * 1024 * 1024);
    const started = performance.now();
    for (let n = 0; n < 2000; n++) {
      expect(pathNameAsWritten(long, 24)).toHaveLength(25);
    }
    // Counting the whole name took some 100 ms for each of these.
    expect(performance.now() - started).toBeLessThan(2000);
    expect(shownAsWritten(long, 64)).toBe(`${"n".repeat(64)}…`);
    expect(hostAsWritten(long)).toBe(`${"n".repeat(255)}…`);
    expect(typedAsWritten(long)).toBe(`${"n".repeat(255)}…`);
  });
});
