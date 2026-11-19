#!/usr/bin/env python3
"""Makes one PDF of the documentation: docs/xilaos.pdf.

The chapters are the files docs/NN-name.md, in the order of their
numbers. They are joined into one document, the links from one chapter
to another become links inside it, and pandoc and weasyprint turn the
result into a PDF.

    python3 tools/docs.py            # needs pandoc and weasyprint
    python3 tools/docs.py --html     # also keeps build/docs/xilaos.html
"""
import datetime
import glob
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS = os.path.join(ROOT, "docs")
OUT = os.path.join(ROOT, "build", "docs")

STYLE = """
@page {
    size: A4;
    margin: 20mm 17mm 20mm 17mm;
    @bottom-center { content: counter(page); font: 8pt "Helvetica Neue", Helvetica, Arial, sans-serif; color: #8a7a5c; }
    @top-right { content: "XilaOS"; font: 8pt "Helvetica Neue", Helvetica, Arial, sans-serif; color: #8a7a5c; }
}
@page :first { @bottom-center { content: none; } @top-right { content: none; } }
html { font: 10pt/1.45 "Helvetica Neue", Helvetica, Arial, sans-serif; color: #2b2418; }
body { margin: 0; }
header#title-block-header { page-break-after: always; padding-top: 70mm; text-align: center; }
header#title-block-header h1.title { font-size: 44pt; margin: 0; letter-spacing: 1pt; color: #5a3d1e; border: 0; }
header#title-block-header p.subtitle { font-size: 14pt; color: #8a5a22; margin: 6mm 0 0 0; }
header#title-block-header p.date { font-size: 10pt; color: #8a7a5c; margin-top: 30mm; }
nav#TOC { page-break-after: always; }
nav#TOC > ul { list-style: none; padding: 0; }
nav#TOC > ul > li { margin-top: 2.5mm; font-weight: 600; }
nav#TOC ul ul { list-style: none; padding-left: 5mm; font-weight: 400; font-size: 9pt; }
nav#TOC a { color: inherit; text-decoration: none; }
nav#TOC a::after { content: leader(".") target-counter(attr(href), page); color: #8a7a5c; }
h1 { font-size: 22pt; color: #5a3d1e; page-break-before: always; margin: 0 0 6mm 0; padding-bottom: 2mm; border-bottom: 1.2pt solid #e0a03c; }
h2 { font-size: 14pt; color: #5a3d1e; margin: 8mm 0 2mm 0; page-break-after: avoid; }
h3 { font-size: 11.5pt; color: #7a4d1c; margin: 6mm 0 1.5mm 0; page-break-after: avoid; }
h4 { font-size: 10pt; margin: 4mm 0 1mm 0; page-break-after: avoid; }
p, li { orphans: 2; widows: 2; }
a { color: #a3541a; text-decoration: none; }
code { font: 8.6pt Menlo, "DejaVu Sans Mono", Consolas, monospace; background: #f6efd9; padding: 0 1.5pt; border-radius: 2pt; }
pre { font: 7.4pt/1.35 Menlo, "DejaVu Sans Mono", Consolas, monospace; background: #f6efd9; border-left: 2pt solid #e0a03c;
      padding: 2.5mm 3mm; white-space: pre-wrap; word-break: break-all; page-break-inside: avoid; }
pre code { font: inherit; background: none; padding: 0; }
table { border-collapse: collapse; width: 100%; margin: 3mm 0; font-size: 8.8pt; page-break-inside: auto; }
tr { page-break-inside: avoid; }
th { text-align: left; background: #f0e3bd; border-bottom: 1pt solid #c9a558; padding: 1.5mm 2mm; }
td { border-bottom: 0.4pt solid #e3d6b0; padding: 1.3mm 2mm; vertical-align: top; }
td code, th code { font-size: 7.8pt; }
img { max-width: 100%; display: block; margin: 3mm auto; border: 0.5pt solid #c9a558; }
figure { margin: 4mm 0; page-break-inside: avoid; }
figcaption { text-align: center; font-size: 8.5pt; color: #8a7a5c; }
blockquote { margin: 3mm 0; padding-left: 4mm; border-left: 2pt solid #c9a558; color: #5d5240; }
"""


def anchor(title):
    """The identifier pandoc gives a heading."""
    kept = re.sub(r"[^\w\- ]", "", title.lower())
    return re.sub(r"\s+", "-", kept.strip())


def main():
    chapters = sorted(glob.glob(os.path.join(DOCS, "[0-9][0-9]-*.md")))
    if not chapters:
        sys.exit("no chapters in docs/")
    texts, anchors = {}, {}
    for path in chapters:
        with open(path, encoding="utf-8") as chapter:
            texts[path] = chapter.read()
        first = re.search(r"^# (.+)$", texts[path], re.MULTILINE)
        if not first:
            sys.exit(os.path.basename(path) + " has no title")
        anchors[os.path.basename(path)] = anchor(first.group(1))

    def inside(match):
        name, part = match.group(1), match.group(2)
        if name not in anchors:
            sys.exit("link to a chapter that does not exist: " + name)
        return "](#" + (part[1:] if part else anchors[name]) + ")"

    whole = "\n\n".join(re.sub(r"\]\(([0-9][0-9]-[a-z0-9-]+\.md)(#[^)]*)?\)", inside, texts[path]) for path in chapters)

    os.makedirs(OUT, exist_ok=True)
    joined, style = os.path.join(OUT, "xilaos.md"), os.path.join(OUT, "xilaos.css")
    with open(joined, "w", encoding="utf-8") as out:
        out.write(whole)
    with open(style, "w", encoding="utf-8") as out:
        out.write(STYLE)
    version = datetime.date.today().isoformat()
    common = ["pandoc", joined, "--from", "gfm", "--standalone", "--toc", "--toc-depth=2", "--resource-path", DOCS,
              "--metadata", "title=XilaOS", "--metadata", "subtitle=A database at the core of a small operating system",
              "--metadata", "date=Documentation, " + version, "--metadata", "lang=en",
              "--css", style]
    pdf = os.path.join(DOCS, "xilaos.pdf")
    subprocess.run(common + ["--pdf-engine", "weasyprint", "--output", pdf], check=True, cwd=DOCS)
    if "--html" in sys.argv:
        subprocess.run(common + ["--embed-resources", "--output", os.path.join(OUT, "xilaos.html")], check=True, cwd=DOCS)
    print(os.path.relpath(pdf, ROOT))


if __name__ == "__main__":
    main()
