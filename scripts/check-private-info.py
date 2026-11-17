#!/usr/bin/env python3
"""Check files before publishing (Buildoffice).

Every file under the given paths is searched, both as bytes (ASCII/UTF-8) and as UTF-16LE, for:

- the current Windows/Unix user name (from USERNAME/USER, if it has 3 or more characters),
- the current home folder path and the source root path (backslash and slash forms),
- any `--needle` passed on the command line,
- a drive letter followed by a folder named Users (with one or more backslashes or slashes around it),
- the shape of a private key block (a BEGIN line and a PRIVATE KEY line together),
- the shape of an access token (GitHub, Anthropic, AWS, Slack, Google API, payment service live keys),
- a file name that looks like a secret (.env, .pem, .key, .pfx, .p12).

Files that start with the gzip signature (for example the app's NOTICES.Z) are decompressed and the
content is searched in the same way.

With `--text-rules` it also reports e-mail addresses and UUIDs (except the UUIDs in files listed in
ALLOWED_UUID_FILES, which are public operating system identifiers).

With `--forbidden-file FILE` it also reports the words listed in FILE (one regular expression per
line; blank lines and lines that start with # are ignored; a line that starts with "text:" is searched
only in text files). The words are searched in the file names, in UTF-8 and in UTF-16LE (also in binary
files and inside gzip files), and in text files also after backslash-u escapes are turned back into
characters. The list file is not part of the published files; without the option this check is skipped.

No private value is written in this file: the values come from the environment at run time.
Exit code 1 if anything is found, 0 otherwise.

Usage: python3 scripts/check-private-info.py [--text-rules] [--forbidden-file FILE] [--needle TEXT]... PATH...
"""

import argparse
import os
import re
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Files whose UUIDs are public operating system identifiers (known folder IDs, manifest OS IDs).
ALLOWED_UUID_FILES = {
    "crates/os-sys/src/win/ffi.rs",
    "app/windows/runner/runner.exe.manifest",
}

DRIVE_USERS = re.compile(rb"(?i)[a-z]:(?:\\|/)+users(?:\\|/)")
EMAIL = re.compile(rb"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
UUID = re.compile(rb"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
# The two halves are joined here so that this line does not match the pattern itself.
PRIVATE_KEY = re.compile(rb"BEGIN[ A-Z]*" + b"PRIV" + b"ATE KEY")
SECRET_NAME = re.compile(r"(?i)^(?:\.env(?:\..*)?|.*\.(?:pem|key|pfx|p12))$")
# The shapes of access tokens (the prefix and a long run of the characters the service uses).
TOKEN_SHAPES = [
    ("GitHub token", re.compile(rb"gh[pousr]_[A-Za-z0-9]{36,}")),
    ("GitHub fine-grained token", re.compile(rb"github_pat_[A-Za-z0-9_]{22,}")),
    ("Anthropic API key", re.compile(rb"sk-ant-[A-Za-z0-9_-]{20,}")),
    ("AWS access key id", re.compile(rb"AKIA[0-9A-Z]{16}")),
    ("Slack token", re.compile(rb"xox[abposr]-[A-Za-z0-9-]{10,}")),
    ("Google API key", re.compile(rb"AIza[0-9A-Za-z_-]{35}")),
    ("payment service live key", re.compile(rb"sk_live_[0-9A-Za-z]{16,}")),
]
# A backslash followed by u and four hexadecimal digits.
UNICODE_ESCAPE = re.compile(r"\\u([0-9a-fA-F]{4})")

# Decompressed gzip content is limited to this size (protection against a compressed bomb).
MAX_GZIP_BYTES = 512 * 1024 * 1024


def path_forms(path):
    text = str(path)
    forms = {text, text.replace("\\", "/"), text.replace("/", "\\")}
    # Git Bash form (/c/...) of a Windows path
    m = re.match(r"^([A-Za-z]):[\\/](.*)$", text)
    if m:
        forms.add("/" + m.group(1).lower() + "/" + m.group(2).replace("\\", "/"))
    return {f for f in forms if len(f) > 3}


def needles(extra):
    found = set(extra)
    for var in ("USERNAME", "USER"):
        name = os.environ.get(var, "")
        if len(name) >= 3:
            found.add(name)
    found |= path_forms(Path.home())
    found |= path_forms(ROOT)
    return sorted(found, key=str.lower)


def gunzip(data):
    """The content of a gzip file, or None if it is not (valid) gzip."""
    if data[:2] != b"\x1f\x8b":
        return None
    try:
        d = zlib.decompressobj(16 + zlib.MAX_WBITS)
        out = d.decompress(data, MAX_GZIP_BYTES)
        if d.unconsumed_tail:
            return None
        return out
    except zlib.error:
        return None


def is_text(data):
    return b"\x00" not in data[:8192]


def load_forbidden(path):
    """The words in a list file: [(compiled regex, text_only)]."""
    rules = []
    for raw in Path(path).read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        text_only = line.startswith("text:")
        if text_only:
            line = line[len("text:") :]
        rules.append((re.compile(line), text_only))
    return rules


def unescape_unicode(text):
    """The text with backslash-u escapes turned back into characters."""
    return UNICODE_ESCAPE.sub(lambda m: chr(int(m.group(1), 16)), text)


def _line_of(text, pos):
    return text.count("\n", 0, pos) + 1


def forbidden_in(text, rules, text_file=True):
    """{pattern: [line numbers]} of the forbidden words found in the text.

    Rules marked text-only are used only when `text_file` is true."""
    found = {}
    for rx, text_only in rules:
        if text_only and not text_file:
            continue
        for m in rx.finditer(text):
            found.setdefault(rx.pattern, []).append(_line_of(text, m.start()))
    return found


def forbidden_views(data, rules):
    """[(view name, {pattern: [line numbers]})] for the views of one file's content.

    Views: UTF-8 text, the same text with backslash-u escapes turned back into characters (text files
    only), and UTF-16LE (both byte alignments). Text-only rules are used for text files (and for UTF-16
    text with a byte order mark)."""
    out = []
    text_file = is_text(data)
    as_text = data.decode("utf-8", "replace")
    out.append(("", forbidden_in(as_text, rules, text_file)))
    if text_file and "\\u" in as_text:
        unescaped = unescape_unicode(as_text)
        if unescaped != as_text:
            out.append(("after turning backslash-u escapes back into characters: ", forbidden_in(unescaped, rules)))
    utf16_text = data[:2] in (b"\xff\xfe", b"\xfe\xff")
    for offset in (0, 1):
        chunk = data[offset:]
        chunk = chunk[: len(chunk) - (len(chunk) % 2)]
        if not chunk:
            continue
        out.append(("utf-16: ", forbidden_in(chunk.decode("utf-16-le", "replace"), rules, utf16_text)))
    return out


def scan(data, words, text_rules, allow_uuid, prefix=""):
    hits = []
    low = data.lower()
    for word in words:
        for label, enc in (("bytes", word.lower().encode("utf-8")), ("utf-16", word.lower().encode("utf-16-le"))):
            if enc and enc in low:
                hits.append(f"{prefix}{label}: {word!r} x{low.count(enc)}")
    variants = [("bytes", data)]
    if b"\x00" in data:
        variants.append(("utf-16", data.replace(b"\x00", b"")))
    for label, blob in variants:
        for m in sorted(set(DRIVE_USERS.findall(blob))):
            hits.append(f"{prefix}{label}: drive+Users {m.decode('ascii', 'replace')!r}")
        if PRIVATE_KEY.search(blob):
            hits.append(f"{prefix}{label}: private key block")
        for kind, rx in TOKEN_SHAPES:
            if rx.search(blob):
                hits.append(f"{prefix}{label}: token-like string ({kind})")
        if text_rules:
            for m in sorted(set(EMAIL.findall(blob))):
                hits.append(f"{prefix}{label}: e-mail {m.decode('ascii', 'replace')!r}")
            if not allow_uuid:
                for m in sorted(set(UUID.findall(blob))):
                    hits.append(f"{prefix}{label}: uuid {m.decode('ascii', 'replace')!r}")
    return hits


def scan_file(rel, data, words, text_rules, rules):
    hits = scan(data, words, text_rules, rel in ALLOWED_UUID_FILES)
    if SECRET_NAME.match(rel.rsplit("/", 1)[-1]):
        hits.append("file name looks like a secret")
    inner = gunzip(data)
    contents = [("", data)]
    if inner is not None:
        hits += scan(inner, words, text_rules, rel in ALLOWED_UUID_FILES, prefix="gzip/")
        contents.append(("gzip/", inner))
    if rules:
        for rx, _text_only in rules:
            if rx.search(rel):
                hits.append(f"forbidden word in the file name: /{rx.pattern}/")
        for prefix, blob in contents:
            for view, found in forbidden_views(blob, rules):
                for pattern, lines in sorted(found.items()):
                    shown = ",".join(str(n) for n in lines[:8]) + (",..." if len(lines) > 8 else "")
                    hits.append(f"{prefix}{view}forbidden word /{pattern}/ x{len(lines)} (line {shown})")
    return hits


def main():
    # The report may show Japanese words; do not depend on the console code page.
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--text-rules", action="store_true", help="also report e-mail addresses and UUIDs")
    parser.add_argument("--forbidden-file", help="also report the words listed in this file")
    parser.add_argument("--needle", action="append", default=[], help="extra text that must not appear")
    parser.add_argument("paths", nargs="+")
    args = parser.parse_args()
    words = needles(args.needle)
    rules = load_forbidden(args.forbidden_file) if args.forbidden_file else []
    if not args.forbidden_file:
        print("note: the check for forbidden words is skipped (no --forbidden-file)")
    bad = 0
    count = 0
    for arg in args.paths:
        base = Path(arg)
        files = [base] if base.is_file() else sorted(p for p in base.rglob("*") if p.is_file())
        for f in files:
            rel_path = f.relative_to(base) if base.is_dir() else Path(f.name)
            if ".git" in rel_path.parts:
                continue
            count += 1
            rel = rel_path.as_posix()
            hits = scan_file(rel, f.read_bytes(), words, args.text_rules, rules)
            if hits:
                bad += 1
                print(f"{rel}: " + "; ".join(hits))
    print(f"checked {count} files with {len(words)} needles and {len(rules)} forbidden words: {bad} file(s) with findings")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
